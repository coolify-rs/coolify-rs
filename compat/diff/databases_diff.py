#!/usr/bin/env python3
"""Differential check of standalone databases: upstream reference vs coolify-rs.

Needs the reference environment, coolify-rs serving on 127.0.0.1:18080 against the
reference database (with APP_KEY and COOLIFY_RS_DATA_DIR), and a real server that
both can reach under the same name and that is validated (team of the fixture tokens),
for example an OrbStack VM with Docker. Usage:

    DATABASE_URL=... APP_KEY=... databases_diff.py --kind redis --server <server uuid> --vm <orbstack machine>

It creates a project, compares create responses, validation errors, the rows each side
writes (with their variables and volumes), show/list responses for several tokens, then
starts one database through each implementation and compares the Docker Compose files
they write on the server, byte for byte. With DATABASE_URL and APP_KEY set it also runs
coolify-rs's compose and configuration hash code over the database upstream created and
compares them with what upstream wrote. Everything it creates is deleted at the end.
Exit status 1 when anything differs.
"""
import argparse
import base64
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


def b64(s):
    return base64.b64encode(s.encode()).decode()


# Per type: table, model, encrypted columns, the request with every field, error requests,
# and the fields the compose comparison sets on both sides so their files can match.
KINDS = {
    "postgresql": {
        "table": "standalone_postgresqls", "model": "StandalonePostgresql", "secrets": ["postgres_password"],
        "rich": {"postgres_conf": b64("max_connections = 50")},
        "errors": [dict(postgres_user="1bad", postgres_password="a b", public_port=80, is_public="x", foo=1),
                   dict(postgres_conf="not base64!"), dict(redis_password="x"), dict(postgres_db="a" * 70)],
        "fixed": {},
    },
    "redis": {
        "table": "standalone_redis", "model": "StandaloneRedis", "secrets": [],
        "rich": {"redis_conf": b64("maxmemory 100mb"), "redis_password": "Rich-Pass_1"},
        "errors": [dict(redis_password="a b", public_port=80, is_public="x", foo=1), dict(redis_conf="not base64!"),
                   dict(postgres_user="x"), dict(redis_password="a" * 130)],
        "fixed": {"redis_password": "Same-Pass_2"},
    },
    "keydb": {
        "table": "standalone_keydbs", "model": "StandaloneKeydb", "secrets": ["keydb_password"],
        "rich": {"keydb_conf": b64("maxmemory 100mb")},
        "errors": [dict(keydb_password="a b", foo=1), dict(keydb_conf="not base64!"), dict(redis_password="x")],
        "fixed": {"keydb_password": "Same-Pass_2"},
    },
    "dragonfly": {
        "table": "standalone_dragonflies", "model": "StandaloneDragonfly", "secrets": ["dragonfly_password"],
        "rich": {"dragonfly_password": "Rich-Pass_1"},
        "errors": [dict(dragonfly_password="a b", foo=1), dict(redis_conf="x")],
        "fixed": {"dragonfly_password": "Same-Pass_2"},
    },
    "mongodb": {
        "table": "standalone_mongodbs", "model": "StandaloneMongodb", "secrets": ["mongo_initdb_root_password"],
        "rich": {"mongo_conf": b64("net:\n  bindIpAll: true\n"), "mongo_initdb_root_username": "admin_u", "mongo_initdb_database": "app_db"},
        "errors": [dict(mongo_initdb_root_username="1bad", mongo_initdb_root_password="a b"), dict(mongo_conf="not base64!"),
                   dict(mysql_user="x")],
        "fixed": {"mongo_initdb_root_password": "Same-Pass_2"},
    },
    "mysql": {
        "table": "standalone_mysqls", "model": "StandaloneMysql", "secrets": ["mysql_root_password", "mysql_password"],
        "rich": {"mysql_conf": b64("[mysqld]\nmax_connections=50\n"), "mysql_user": "app_u", "mysql_database": "app_db"},
        "errors": [dict(mysql_user="1bad", mysql_password="a b", mysql_root_password=""), dict(mysql_conf="not base64!"),
                   dict(mariadb_user="x")],
        "fixed": {"mysql_password": "Same-Pass_2", "mysql_root_password": "Same-Root_3"},
    },
    "mariadb": {
        "table": "standalone_mariadbs", "model": "StandaloneMariadb", "secrets": ["mariadb_root_password", "mariadb_password"],
        "rich": {"mariadb_conf": b64("[mysqld]\nmax_connections=50\n"), "mariadb_user": "app_u", "mariadb_database": "app_db"},
        "errors": [dict(mariadb_user="1bad", mariadb_password="a b"), dict(mariadb_conf="not base64!"), dict(mysql_user="x")],
        "fixed": {"mariadb_password": "Same-Pass_2", "mariadb_root_password": "Same-Root_3"},
    },
    "clickhouse": {
        "table": "standalone_clickhouses", "model": "StandaloneClickhouse", "secrets": ["clickhouse_admin_password"],
        "rich": {"clickhouse_admin_user": "admin_u"},
        "errors": [dict(clickhouse_admin_user="1bad", clickhouse_admin_password="a b"), dict(clickhouse_conf="x")],
        "fixed": {"clickhouse_admin_password": "Same-Pass_2"},
    },
}


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
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"\n  {detail}"[:3000]))


def json_diff(a, b):
    """The status codes and the keys whose values or order differ."""
    try:
        ja, jb = json.loads(a[1]), json.loads(b[1])
    except ValueError:
        return f"ref {a}\n  rs  {b}"
    if not (isinstance(ja, dict) and isinstance(jb, dict)):
        return f"ref {a}\n  rs  {b}"
    keys = [k for k in dict.fromkeys([*ja, *jb]) if ja.get(k) != jb.get(k)]
    out = [f"status {a[0]} / {b[0]}"] + [f"{k}: {json.dumps(ja.get(k))[:300]} | {json.dumps(jb.get(k))[:300]}" for k in keys]
    if list(ja) != list(jb):
        out.append(f"key order differs: {list(ja)} / {list(jb)}")
    return "\n  ".join(out)


def same(name, method, path, body=None, tok="acme_owner_write"):
    a, b = call("ref", method, path, body, tok), call("rs", method, path, body, tok)
    record(name, a == b, f"ref {a}\n  rs  {b}")
    return a


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--kind", default="postgresql", choices=sorted(KINDS))
    ap.add_argument("--server", required=True)
    ap.add_argument("--vm", required=True)
    a = ap.parse_args()
    k = KINDS[a.kind]
    table, morph = k["table"], "App\\Models\\" + k["model"]
    endpoint = f"/api/v1/databases/{a.kind}"

    def vm(cmd):
        return subprocess.run(["orb", "-m", a.vm, "-u", "root", "bash", "-c", cmd], capture_output=True, text=True).stdout

    def status(u):
        return sql(f"SELECT status FROM {table} WHERE uuid='{u}'")

    def rows(uuid, other):
        r = json.loads(sql(f"SELECT row_to_json(d)::text FROM {table} d WHERE uuid='{uuid}'"))
        rid = r["id"]
        vols = [json.loads(x) for x in sql(f"SELECT row_to_json(v)::text FROM local_persistent_volumes v WHERE resource_type='{morph}' "
                                           f"AND resource_id={rid} ORDER BY id").splitlines()]
        envs = [json.loads(x) for x in sql(f"SELECT row_to_json(v)::text FROM environment_variables v WHERE resourceable_type='{morph}' "
                                           f"AND resourceable_id={rid} ORDER BY id").splitlines()]
        for key in ["id", "uuid", "created_at", "updated_at", *k["secrets"]]:
            r.pop(key)
        for v in vols:
            for key in ["id", "uuid", "resource_id", "created_at", "updated_at"]:
                v.pop(key)
            v["name"] = v["name"].replace(uuid, "U").replace(other, "U")
        for v in envs:
            for key in ["id", "uuid", "resourceable_id", "created_at", "updated_at", "value"]:
                v.pop(key)
        return r, vols, envs

    project = json.loads(call("ref", "POST", "/api/v1/projects", {"name": "databases-diff"})[1])["uuid"]
    base = {"server_uuid": a.server, "project_uuid": project, "environment_name": "production"}
    created = []
    try:
        body = dict(base, name="diff <b>db</b>", description="d", limits_memory=512, limits_cpu_shares="2048",
                    is_public=True, public_port_timeout=60, **k["rich"])
        ca, ra = call("ref", "POST", endpoint, body, tok="acme_owner_root")
        cb, rb = call("rs", "POST", endpoint, body, tok="acme_owner_root")
        ua, ub = json.loads(ra)["uuid"], json.loads(rb)["uuid"]
        created += [ua, ub]
        ja, jb = json.loads(ra), json.loads(rb)
        record("create status and shape", ca == cb == 201 and ja.keys() == jb.keys(), f"{ca} {ra} / {cb} {rb}")
        if "internal_db_url" in ja and a.kind == "redis":
            record("created internal_db_url alike", ja["internal_db_url"].replace(ua, "U") == jb.get("internal_db_url", "").replace(ub, "U"),
                   f"{ja} / {jb}")
        (rowa, vola, enva), (rowb, volb, envb) = rows(ua, ub), rows(ub, ua)
        record("rows written alike", rowa == rowb, {x: (rowa[x], rowb.get(x)) for x in rowa if rowa[x] != rowb.get(x)})
        record("data volume rows alike", vola == volb, f"{vola} / {volb}")
        record("variable rows alike", enva == envb, f"{enva} / {envb}")

        for i, b in enumerate([{}, dict(base, project_uuid="nope"), dict(base, server_uuid="nope"), dict(base, environment_name="nope"),
                               dict(base, public_port=80), dict(base, name=5, limits_memory_swappiness="x", public_port_timeout=0),
                               dict(base, tags="x"), {"server_uuid": a.server, "project_uuid": project}]
                              + [dict(base, **e) for e in k["errors"]]):
            same(f"create error {i}", "POST", endpoint, b)
        same("instant deploy without deploy ability", "POST", endpoint, dict(base, instant_deploy=True))
        same("member cannot create", "POST", endpoint, base, tok="acme_member_write_legacy")

        for tok in ["acme_owner_read", "acme_owner_root", "acme_member_read"]:
            for u in (ua, ub):
                ra_, rb_ = call("ref", "GET", f"/api/v1/databases/{u}", tok=tok), call("rs", "GET", f"/api/v1/databases/{u}", tok=tok)
                record(f"show {u[:6]} [{tok}]", ra_ == rb_, json_diff(ra_, rb_))
            same(f"list [{tok}]", "GET", "/api/v1/databases", tok=tok)
        same("show missing", "GET", "/api/v1/databases/nope", tok="acme_owner_read")
        for action in ["start", "stop", "restart"]:
            same(f"{action} with GET", "GET", f"/api/v1/databases/{ua}/{action}", tok="acme_owner_root")
            same(f"{action} without deploy", "POST", f"/api/v1/databases/{ua}/{action}")
        same("stop a stopped database", "POST", f"/api/v1/databases/{ua}/stop", tok="acme_owner_root")
        same("delete without deploy", "DELETE", f"/api/v1/databases/{ua}")

        # Start one plain database through each side and compare the files on the server.
        compose = {}
        for side in ("ref", "rs"):
            u = json.loads(call(side, "POST", endpoint, dict(base, name="compose-diff", **k["fixed"]))[1])["uuid"]
            created.append(u)
            call(side, "POST", f"/api/v1/databases/{u}/start", tok="acme_owner_root")
            for _ in range(90):
                if status(u) == "running:healthy":
                    break
                time.sleep(2)
            compose[side] = (u, vm(f"cat /data/coolify/databases/{u}/docker-compose.yml"))
        (ur, fr), (us, fs) = compose["ref"], compose["rs"]
        record("both started and healthy", all(status(u) == "running:healthy" for u in (ur, us)), f"{status(ur)} / {status(us)}")

        # Each file names its own database and maybe its generated password; everything else must match.
        def norm(text, u):
            out = []
            for line in text.replace(u, "UUID").splitlines():
                if "PASSWORD=" in line and not k["fixed"]:
                    line = line.split("PASSWORD=")[0] + "PASSWORD=<secret>"
                out.append(line)
            return "\n".join(out)
        record("docker-compose.yml identical (UUIDs normalized)", norm(fr, ur) == norm(fs, us),
               "\n".join(difflib.unified_diff(norm(fr, ur).splitlines(), norm(fs, us).splitlines(), lineterm="")))
        (_, _, er), (_, _, es) = rows(ur, us), rows(us, ur)
        record("variable rows alike after start", er == es, f"{er} / {es}")

        # coolify-rs's own code over the rows upstream wrote.
        if os.environ.get("DATABASE_URL") and os.environ.get("APP_KEY"):
            team = sql(f"SELECT p.team_id FROM {table} d JOIN environments e ON e.id = d.environment_id "
                       f"JOIN projects p ON p.id = e.project_id WHERE d.uuid='{ur}'")
            def example(*extra):
                return subprocess.run(["cargo", "run", "-q", "-p", "coolify-jobs", "--example", "database_compose", "--", team, ur, *extra],
                                      cwd=ROOT, capture_output=True, text=True).stdout
            # Upstream saves the hash from the UI only; ask the model for it.
            stored = subprocess.run(["docker", "exec", "coolify-rs-reference-coolify-1", "php", "/var/www/html/artisan", "tinker", "--execute",
                                     f"$d = App\\Models\\{k['model']}::where('uuid', '{ur}')->first(); $d->isConfigurationChanged(true); "
                                     f"echo $d->fresh()->config_hash;"], capture_output=True, text=True).stdout.strip().splitlines()[-1:]
            stored = stored[0] if stored else ""
            ours = example("--hash").strip()
            record("config_hash of upstream's database", stored == ours, f"upstream {stored} / coolify-rs {ours}")
            mine = example()
            record("compose for upstream's database identical", mine == fr,
                   "\n".join(difflib.unified_diff(fr.splitlines(), mine.splitlines(), lineterm="")))
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
