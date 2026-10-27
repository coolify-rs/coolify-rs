#!/usr/bin/env python3
"""Differential check of standalone PostgreSQL databases: upstream reference vs coolify-rs.

Needs the reference environment, coolify-rs serving on 127.0.0.1:18080 against the
reference database (with APP_KEY and COOLIFY_RS_DATA_DIR), and a real server that
both can reach under the same name and that is validated (team of the fixture tokens),
for example an OrbStack VM with Docker. Usage:

    databases_diff.py --server <server uuid> --vm <orbstack machine name>

It creates a project, compares create responses, validation errors, the rows each side
writes, show/list responses for several tokens, then starts one database through each
implementation and compares the Docker Compose files they write on the server, byte for
byte. Everything it creates is deleted at the end. Exit status 1 when anything differs.
"""
import argparse
import difflib
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
TOKENS = json.load(open(os.path.join(ROOT, "compat/reference/secrets/tokens.json")))
BASES = {"ref": "http://127.0.0.1:18000", "rs": "http://127.0.0.1:18080"}
results = []


def call(side, method, path, body=None, tok="acme_owner_write"):
    data = json.dumps(body).encode() if body is not None else None
    for _ in range(5):
        r = urllib.request.Request(BASES[side] + path, data=data, method=method)
        r.add_header("Authorization", "Bearer " + TOKENS[tok])
        r.add_header("Accept", "application/json")
        if data:
            r.add_header("Content-Type", "application/json")
        try:
            resp = urllib.request.urlopen(r)
            return resp.status, resp.read()
        except urllib.error.HTTPError as e:
            if e.code == 429:
                time.sleep(int(e.headers.get("Retry-After", "5")))
                continue
            return e.code, e.read()
    raise SystemExit("rate limited")


def sql(query):
    out = subprocess.run(["docker", "exec", "coolify-rs-reference-postgres-1", "psql", "-U", "coolify", "-d", "coolify", "-At", "-c", query],
                         check=True, capture_output=True, text=True).stdout
    return out.strip()


def record(name, ok, detail=""):
    results.append(ok)
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"\n  {detail}"[:2000]))


def same(name, method, path, body=None, tok="acme_owner_write"):
    a, b = call("ref", method, path, body, tok), call("rs", method, path, body, tok)
    record(name, a == b, f"ref {a}\n  rs  {b}")
    return a


def row(uuid, other):
    r = json.loads(sql(f"SELECT row_to_json(d)::text FROM standalone_postgresqls d WHERE uuid='{uuid}'"))
    v = json.loads(sql("SELECT row_to_json(v)::text FROM local_persistent_volumes v WHERE resource_type='App\\Models\\StandalonePostgresql' "
                       f"AND resource_id={r['id']}"))
    for k in ["id", "uuid", "created_at", "updated_at", "postgres_password"]:
        r.pop(k)
    for k in ["id", "uuid", "resource_id", "created_at", "updated_at"]:
        v.pop(k)
    v["name"] = v["name"].replace(uuid, "U").replace(other, "U")
    return r, v


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", required=True)
    ap.add_argument("--vm", required=True)
    a = ap.parse_args()

    def vm(cmd):
        return subprocess.run(["orb", "-m", a.vm, "-u", "root", "bash", "-c", cmd], capture_output=True, text=True).stdout

    project = json.loads(call("ref", "POST", "/api/v1/projects", {"name": "databases-diff"})[1])["uuid"]
    base = {"server_uuid": a.server, "project_uuid": project, "environment_name": "production"}
    created = []
    try:
        body = dict(base, name="diff <b>db</b>", description="d", limits_memory=512, limits_cpu_shares="2048",
                    is_public=True, postgres_conf="bWF4X2Nvbm5lY3Rpb25zID0gNTA=", public_port_timeout=60)
        ca, ra = call("ref", "POST", "/api/v1/databases/postgresql", body)
        cb, rb = call("rs", "POST", "/api/v1/databases/postgresql", body)
        ua, ub = json.loads(ra)["uuid"], json.loads(rb)["uuid"]
        created += [ua, ub]
        record("create status and shape", ca == cb == 201 and json.loads(ra).keys() == json.loads(rb).keys(), f"{ca} {ra} / {cb} {rb}")
        (rowa, vola), (rowb, volb) = row(ua, ub), row(ub, ua)
        record("rows written alike", rowa == rowb, {k: (rowa[k], rowb.get(k)) for k in rowa if rowa[k] != rowb.get(k)})
        record("data volume rows alike", vola == volb, f"{vola} / {volb}")

        for i, b in enumerate([{}, dict(base, project_uuid="nope"), dict(base, server_uuid="nope"), dict(base, environment_name="nope"),
                               dict(base, postgres_user="1bad", postgres_password="a b", public_port=80, is_public="x", foo=1),
                               dict(base, public_port=80), dict(base, postgres_conf="not base64!"), dict(base, redis_password="x"),
                               dict(base, name=5, limits_memory_swappiness="x", public_port_timeout=0), dict(base, tags="x"),
                               dict(base, postgres_db="a" * 70), {"server_uuid": a.server, "project_uuid": project}]):
            same(f"create error {i}", "POST", "/api/v1/databases/postgresql", b)
        same("instant deploy without deploy ability", "POST", "/api/v1/databases/postgresql", dict(base, instant_deploy=True))
        same("member cannot create", "POST", "/api/v1/databases/postgresql", base, tok="acme_member_write_legacy")

        for tok in ["acme_owner_read", "acme_owner_root", "acme_member_read"]:
            for u in (ua, ub):
                same(f"show {u[:6]} [{tok}]", "GET", f"/api/v1/databases/{u}", tok=tok)
            same(f"list [{tok}]", "GET", "/api/v1/databases", tok=tok)
        same("show missing", "GET", "/api/v1/databases/nope", tok="acme_owner_read")
        for action in ["start", "stop", "restart"]:
            same(f"{action} with GET", "GET", f"/api/v1/databases/{ua}/{action}", tok="acme_owner_root")
            same(f"{action} without deploy", "POST", f"/api/v1/databases/{ua}/{action}")
            same(f"{action} missing", "POST", f"/api/v1/databases/nope/{action}", tok="acme_owner_root")
        same("stop a stopped database", "POST", f"/api/v1/databases/{ua}/stop", tok="acme_owner_root")
        same("delete without deploy", "DELETE", f"/api/v1/databases/{ua}")
        same("delete missing", "DELETE", "/api/v1/databases/nope", tok="acme_owner_root")

        # Start one plain database through each side and compare the files on the server.
        compose = {}
        for side in ("ref", "rs"):
            u = json.loads(call(side, "POST", "/api/v1/databases/postgresql", dict(base, name="compose-diff"))[1])["uuid"]
            created.append(u)
            call(side, "POST", f"/api/v1/databases/{u}/start", tok="acme_owner_root")
            for _ in range(90):
                if sql(f"SELECT status FROM standalone_postgresqls WHERE uuid='{u}'") == "running:healthy":
                    break
                time.sleep(2)
            compose[side] = (u, vm(f"cat /data/coolify/databases/{u}/docker-compose.yml"))
        (ur, fr), (us, fs) = compose["ref"], compose["rs"]
        record("both started and healthy", all(sql(f"SELECT status FROM standalone_postgresqls WHERE uuid='{u}'") == "running:healthy" for u in (ur, us)))
        # Each file names its own database and its generated password; everything else must match.
        def norm(text, u):
            out = []
            for line in text.replace(u, "UUID").splitlines():
                if "PASSWORD=" in line:
                    line = line.split("PASSWORD=")[0] + "PASSWORD=<secret>"
                out.append(line)
            return "\n".join(out)
        record("docker-compose.yml identical (UUIDs and generated passwords normalized)", norm(fr, ur) == norm(fs, us),
               "\n".join(difflib.unified_diff(norm(fr, ur).splitlines(), norm(fs, us).splitlines(), lineterm="")))
    finally:
        for u in created:
            call("ref", "DELETE", f"/api/v1/databases/{u}", tok="acme_owner_root")
        time.sleep(5)
        call("ref", "DELETE", f"/api/v1/projects/{project}", tok="acme_owner_root")
    passed = sum(results)
    print(f"\n{passed}/{len(results)} checks identical")
    sys.exit(0 if passed == len(results) else 1)


if __name__ == "__main__":
    main()
