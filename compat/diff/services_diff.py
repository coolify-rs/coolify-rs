#!/usr/bin/env python3
"""Differential check of one-click services: upstream reference vs coolify-rs.

Needs the reference environment and coolify-rs on COOLIFY_RS_BASE against the reference database.
Usage:

    COOLIFY_RS_BASE=http://127.0.0.1:18081 services_diff.py [--all-templates] [--no-live] [--only name]

Creates services from templates and from custom compose files on both sides and compares the
responses, every row (services, parts, variables, storages), the generated compose, validation and
error paths and all sub-routes. Generated secrets compare by key, length and character class. The
live part deploys on the test VM and compares the files under /data/coolify/services/{uuid},
containers, status after polling, HTTP through the proxy, then stops, restarts and deletes.
Everything it creates is deleted at the end. Exit status 1 when anything differs.
"""
import argparse
import base64
import json
import os
import re
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from databases_diff import BASES, call, record, results, sql  # noqa: E402

SERVER = "hml20fsj5eles0xfiqidq8j5"
VM = "coolify-rs-e2e"
ROOT_TOKEN = "acme_owner_root"
ENV_FILE = "." + "env"
TS = re.compile(r"\d{4}-\d\d-\d\d[T ]\d\d:\d\d:\d\d(\.\d+Z?)?")
GENERATED = re.compile(r"^SERVICE_(PASSWORD|PASSWORDWITHSYMBOLS|USER|BASE64|REALBASE64|HEX|LOWERCASEUSER|SUPABASE)")

TEMPLATES = ["uptime-kuma", "wordpress-with-mariadb", "budibase", "logto", "garage", "pocketbase", "n8n-with-postgresql",
             "appwrite", "beszel", "pgadmin", "supabase", "plane", "ghost", "code-server", "redis-insight", "denoKV"]

CUSTOM = {
    "basic": """services:
  web:
    image: nginx:1.27-alpine
    environment:
      - SERVICE_URL_WEB_80
      - PLAIN=value
      - REQUIRED=${REQUIRED:?must be set}
      - WITH_DEFAULT=${WITH_DEFAULT:-fallback} # a comment
      - NESTED=${OUTER:-${INNER:-deep}}
      - SECRET=$SERVICE_PASSWORD_WEB
      - USERNAME=$SERVICE_USER_WEB
      - PASSTHROUGH
    volumes:
      - data:/usr/share/nginx/html
      - ./conf:/etc/extra
      - type: bind
        source: ./hello.txt
        target: /usr/share/nginx/hello.txt
        content: "hello world"
    labels:
      - my.label=$dollar
  cache:
    image: redis:7-alpine
    command: redis-server --save ""
    volumes:
      - cache:/data
    restart: "no"
volumes:
  data:
  cache:
""",
    "map_env": """services:
  app:
    image: traefik/whoami:v1.10
    environment:
      SERVICE_FQDN_APP: /sub
      FOO: bar
      NUMBER: 3
      EMPTY: ""
      NULLISH:
      URL: ${SERVICE_URL_APP}
      SHARED: "{{team.NOPE}}"
    ports:
      - "18999:80"
    exclude_from_hc: true
  db:
    image: postgres:16-alpine
    environment:
      POSTGRES_PASSWORD: $SERVICE_PASSWORD_POSTGRES
      POSTGRES_USER: $SERVICE_USER_POSTGRES
    volumes:
      - pg:/var/lib/postgresql/data
    networks:
      - extra
networks:
  extra:
    name: ${EXTRA_NET:-my-extra}
volumes:
  pg:
    driver_opts:
      type: none
      o: bind
      device: /tmp
""",
}


def b64(s):
    return base64.b64encode(s.encode()).decode()


def vm(cmd):
    return subprocess.run(["orb", "-m", VM, "-u", "root", "bash", "-c", cmd], capture_output=True, text=True).stdout


def api(side, method, path, body=None, tok=ROOT_TOKEN):
    for attempt in range(6):
        try:
            status, raw = call(side, method, path, body, tok)
            break
        except SystemExit:
            time.sleep(30)
    else:
        raise SystemExit("rate limited")
    try:
        return status, json.loads(raw) if raw else None
    except ValueError:
        return status, raw.decode(errors="replace")


def shape(value):
    # A random string may happen to lack digits: the class is its alphabet, not the letters drawn.
    v = value or ""
    alphabet = "symbols" if re.search(r"[^a-zA-Z0-9+/=_.-]", v) else "alnum"
    return f"<secret len={len(v)} {alphabet}>"


class Scrub:
    def __init__(self, uuid):
        self.map = {uuid: "{S}"}
        self.composed = {}
        for prefix, table in (("A", "service_applications"), ("D", "service_databases")):
            for line in sql(f"SELECT x.uuid, x.name FROM {table} x JOIN services s ON s.id = x.service_id WHERE s.uuid='{uuid}'").splitlines():
                u, n = line.split("|", 1)
                self.map[u] = "{" + f"{prefix}:{n}" + "}"

    def text(self, s):
        for k, v in self.map.items():
            s = s.replace(k, v)
        return TS.sub("<ts>", s)

    def value(self, v, drop=()):
        if isinstance(v, dict):
            return {k: self.value(x, drop) for k, x in v.items() if k not in drop}
        if isinstance(v, list):
            return [self.value(x, drop) for x in v]
        if isinstance(v, str):
            return self.text(v)
        return v


def rows(uuid):
    """Every row of a service, in insertion order, without ids and encrypted values."""
    s = json.loads(sql(f"SELECT row_to_json(s)::text FROM services s WHERE uuid='{uuid}'"))
    sid = s["id"]
    out = {"service": {k: v for k, v in s.items() if k not in ("id", "environment_id", "config_hash", "docker_compose", "docker_compose_raw", "server_id", "destination_id")}}
    out["raw_len"] = len(s["docker_compose_raw"] or "")
    for key, table, morph in (("applications", "service_applications", "App\\Models\\ServiceApplication"), ("databases", "service_databases", "App\\Models\\ServiceDatabase")):
        parts = [json.loads(x) for x in sql(f"SELECT row_to_json(x)::text FROM {table} x WHERE service_id={sid} ORDER BY name").splitlines()]
        for p in parts:
            pid = p["id"]
            p["persistent"] = [json.loads(x) for x in sql(f"SELECT row_to_json(v)::text FROM local_persistent_volumes v WHERE resource_type='{morph}' AND resource_id={pid} ORDER BY name, mount_path").splitlines()]
            p["files"] = [json.loads(x) for x in sql(f"SELECT row_to_json(v)::text FROM local_file_volumes v WHERE resource_type='{morph}' AND resource_id={pid} ORDER BY mount_path").splitlines()]
            for lst in (p["persistent"], p["files"]):
                for v in lst:
                    for k in ("id", "uuid", "resource_id", "created_at", "updated_at"):
                        v.pop(k, None)
                    if v.get("content"):
                        v["content"] = "<encrypted>"
            for k in ("id", "service_id", "last_online_at", "uuid"):
                p.pop(k, None)
        out[key] = parts
    return out, sid


def norm_env(lst):
    out = {}
    for e in lst:
        e = dict(e)
        key = e.get("key", "")
        if GENERATED.match(key):
            for f in ("value", "real_value"):
                if f in e:
                    e[f] = shape(e[f])
        out[key] = e
    return out


def by_key(env):
    """Order-independent view: rows updated later move in Postgres's heap order."""
    return dict(sorted(env.items()))


def deep_diff(a, b, path=""):
    if isinstance(a, dict) and isinstance(b, dict):
        out = []
        for k in dict.fromkeys([*a, *b]):
            if k not in a or k not in b:
                out.append(f"{path}/{k}: only on {'rs' if k not in a else 'ref'}: {json.dumps(a.get(k, b.get(k)), default=str)[:300]}")
            else:
                out += deep_diff(a[k], b[k], f"{path}/{k}")
        if not out and list(a) != list(b):
            out.append(f"{path}: key order {list(a)} / {list(b)}")
        return out
    if isinstance(a, list) and isinstance(b, list):
        if len(a) != len(b):
            return [f"{path}: length {len(a)} / {len(b)}: {json.dumps(a, default=str)[:600]} | {json.dumps(b, default=str)[:600]}"]
        out = []
        for i, (x, y) in enumerate(zip(a, b)):
            out += deep_diff(x, y, f"{path}/{i}")
        return out
    if a != b:
        return [f"{path}: {json.dumps(a, default=str)[:600]} | {json.dumps(b, default=str)[:600]}"]
    return []


def compare(name, a, b):
    if a == b and deep_diff(a, b) == []:
        record(name, True)
        return True
    record(name, False, "\n  ".join(deep_diff(a, b)[:15]))
    return False


def same_api(name, method, path_ref, body=None, tok=ROOT_TOKEN, scrub=None, path_rs=None, drop=()):
    a = api("ref", method, path_ref, body, tok)
    b = api("rs", method, path_rs or path_ref, body, tok)
    sa, sb = scrub or (None, None)
    va = sa.value(a[1], drop) if sa else a[1]
    vb = sb.value(b[1], drop) if sb else b[1]
    return compare(name, [a[0], va], [b[0], vb]), a, b


def compose_view(text, scrub):
    text = text or ""
    for k, v in scrub.composed.items():
        text = text.replace(k, v)
    return scrub.text(text)


def by_name(v):
    """`applications` and `databases` have no ORDER BY upstream: Postgres returns them in heap order."""
    if isinstance(v, dict):
        for k in ("applications", "databases"):
            if isinstance(v.get(k), list):
                v[k] = sorted(v[k], key=lambda x: x.get("name", ""))
    return v


def compare_service(name, ur, urs, tokens=(ROOT_TOKEN, "acme_owner_read", "acme_member_read")):
    sa, sb = Scrub(ur), Scrub(urs)
    # The storage jobs run in the background on both sides and change these rows: let them finish.
    for _ in range(10):
        (ra, _), (rb, _) = rows(ur), rows(urs)
        if sa.value(ra) == sb.value(rb):
            break
        time.sleep(3)
    compare(f"{name}: rows", sa.value(ra), sb.value(rb))
    ca = sql(f"SELECT docker_compose FROM services WHERE uuid='{ur}'")
    cb = sql(f"SELECT docker_compose FROM services WHERE uuid='{urs}'")
    # A bare `- SERVICE_USER_X` puts the generated value into the compose file.
    for scrub, u, side in ((sa, ur, "ref"), (sb, urs, "rs")):
        for e in api(side, "GET", f"/api/v1/services/{u}/envs")[1] or []:
            if GENERATED.match(e.get("key", "")) and e.get("value"):
                scrub.composed[e["value"]] = "<" + e["key"] + ">"
    ok = compose_view(ca, sa) == compose_view(cb, sb)
    if not ok:
        la, lb = compose_view(ca, sa).splitlines(), compose_view(cb, sb).splitlines()
        first = next((i for i, (x, y) in enumerate(zip(la, lb)) if x != y), min(len(la), len(lb)))
        record(f"{name}: docker_compose", False, f"line {first}\n  ref {la[first:first + 4]}\n  rs  {lb[first:first + 4]}")
    else:
        record(f"{name}: docker_compose", True)
    ea = api("ref", "GET", f"/api/v1/services/{ur}/envs")
    eb = api("rs", "GET", f"/api/v1/services/{urs}/envs")
    drop = ("uuid", "created_at", "updated_at")
    compare(f"{name}: envs", [ea[0], by_key(norm_env(sa.value(ea[1], drop)))], [eb[0], by_key(norm_env(sb.value(eb[1], drop)))])
    for tok in tokens:
        a = api("ref", "GET", f"/api/v1/services/{ur}", tok=tok)
        b = api("rs", "GET", f"/api/v1/services/{urs}", tok=tok)
        d = ("config_hash", "docker_compose", "docker_compose_raw", "environment_id", "id", "server_id", "destination_id", "service_id", "last_online_at", "uuid", "ip")
        compare(f"{name}: show {tok}", [a[0], by_name(sa.value(a[1], d))], [b[0], by_name(sb.value(b[1], d))])


def create_pair(name, body, project):
    full = {"project_uuid": project, "environment_name": "production", "server_uuid": SERVER, **body}
    a = api("ref", "POST", "/api/v1/services", full)
    b = api("rs", "POST", "/api/v1/services", full)
    ua = (a[1] or {}).get("uuid") if isinstance(a[1], dict) else None
    ub = (b[1] or {}).get("uuid") if isinstance(b[1], dict) else None
    if not (ua and ub):
        compare(f"{name}: create", list(a), list(b))
        for u, side in ((ua, "ref"), (ub, "rs")):
            if u:
                remove(side, u)
        return None, None
    sa, sb = Scrub(ua), Scrub(ub)
    compare(f"{name}: create", [a[0], sa.value(a[1], ("uuid",))], [b[0], sb.value(b[1], ("uuid",))])
    return ua, ub


def remove(side, uuid):
    api(side, "DELETE", f"/api/v1/services/{uuid}")


def wait_gone(uuids, timeout=120):
    end = time.time() + timeout
    while time.time() < end:
        if not any(sql(f"SELECT 1 FROM services WHERE uuid='{u}'") for u in uuids):
            return True
        time.sleep(2)
    return False


def errors(project):
    base = {"project_uuid": project, "environment_name": "production", "server_uuid": SERVER}
    cases = [
        ("no type or compose", dict(base)),
        ("both", {**base, "type": "ghost", "docker_compose_raw": b64("services: {}")}),
        ("extra field", {**base, "type": "ghost", "foo": 1}),
        ("no env", {"project_uuid": project, "server_uuid": SERVER, "type": "ghost"}),
        ("bad project", {**base, "project_uuid": "nope", "type": "ghost"}),
        ("bad environment", {**base, "environment_name": "nope", "type": "ghost"}),
        ("bad server", {**base, "server_uuid": "nope", "type": "ghost"}),
        ("bad type", {**base, "type": "not-a-template"}),
        ("not base64", {**base, "docker_compose_raw": "not base64!"}),
        ("injection", {**base, "docker_compose_raw": b64("services:\n  'a;b':\n    image: x\n")}),
        ("bad urls", {**base, "type": "ghost", "urls": "x"}),
        ("bad urls item", {**base, "type": "ghost", "urls": [{"name": "a", "x": 1}]}),
        ("bad tags", {**base, "type": "ghost", "tags": ["a"]}),
        ("wrong name type", {**base, "type": "ghost", "name": 1}),
    ]
    for name, body in cases:
        same_api(f"create error: {name}", "POST", "/api/v1/services", body)
    a = api("ref", "POST", "/api/v1/services", {**base, "type": "ghost"}, "acme_member_read")
    b = api("rs", "POST", "/api/v1/services", {**base, "type": "ghost"}, "acme_member_read")
    compare("create with a read token", list(a), list(b))
    for method, path, body in (("GET", "/api/v1/services/nope", None), ("PATCH", "/api/v1/services/nope", {"name": "x"}),
                               ("DELETE", "/api/v1/services/nope", None), ("GET", "/api/v1/services/nope/envs", None),
                               ("POST", "/api/v1/services/nope/start", None), ("GET", "/api/v1/services/nope/start", None),
                               ("GET", "/api/v1/services/nope/logs", None), ("GET", "/api/v1/services/nope/applications", None),
                               ("GET", "/api/v1/services/nope/databases", None), ("GET", "/api/v1/services/nope/tags", None),
                               ("GET", "/api/v1/services/nope/storages", None), ("GET", "/api/v1/services/nope/scheduled-tasks", None)):
        same_api(f"{method} {path}", method, path, body)


def subroutes(name, ur, urs):
    sa, sb = Scrub(ur), Scrub(urs)
    # Each side gets its own domain below, so that they do not conflict.
    sa.map[f"ref-{name}"] = f"x-{name}"
    sb.map[f"rs-{name}"] = f"x-{name}"
    both = (sa, sb)
    P = "/api/v1/services"
    drop = ("uuid", "id", "created_at", "updated_at", "resource_id", "last_online_at", "service_id")

    def s2(label, method, sub, body=None, tok=ROOT_TOKEN, d=drop):
        return same_api(f"{name}: {label}", method, f"{P}/{ur}{sub}", body, tok, both, f"{P}/{urs}{sub}", d)

    # Update and validation
    s2("patch empty name type", "PATCH", "", {"name": 1})
    s2("patch extra", "PATCH", "", {"foo": 1})
    s2("patch name", "PATCH", "", {"name": f"renamed-{name}", "description": "<b>desc</b>"})
    s2("patch bad compose", "PATCH", "", {"docker_compose_raw": "not base64!"})
    # Envs
    s2("env create", "POST", "/envs", {"key": "API_VAR", "value": "one", "comment": "note"}, d=drop)
    s2("env duplicate", "POST", "/envs", {"key": "API_VAR", "value": "two"})
    s2("env bad key", "POST", "/envs", {"key": "1 bad", "value": "x"})
    s2("env update", "PATCH", "/envs", {"key": "API_VAR", "value": "changed", "is_literal": True})
    s2("env update missing", "PATCH", "/envs", {"key": "NOPE", "value": "x"})
    s2("env bulk", "PATCH", "/envs/bulk", {"data": [{"key": "BULK_A", "value": "a"}, {"key": "API_VAR", "value": "b", "is_multiline": True}]})
    s2("env bulk empty", "PATCH", "/envs/bulk", {})
    s2("env read token", "GET", "/envs", tok="acme_member_read")
    euuid = lambda u: next(e["uuid"] for e in api("ref" if u == ur else "rs", "GET", f"{P}/{u}/envs")[1] if e["key"] == "BULK_A")
    a = api("ref", "DELETE", f"{P}/{ur}/envs/{euuid(ur)}")
    b = api("rs", "DELETE", f"{P}/{urs}/envs/{euuid(urs)}")
    compare(f"{name}: env delete", list(a), list(b))
    s2("env delete missing", "DELETE", "/envs/nope")
    # Tags
    s2("tag create", "POST", "/tags", {"tag_name": "Svc-Diff"})
    s2("tag both", "POST", "/tags", {"tag_name": "a1", "tag_names": ["b1"]})
    s2("tag short", "POST", "/tags", {"tag_name": "a"})
    s2("tag list", "GET", "/tags")
    tid = api("ref", "GET", f"{P}/{ur}/tags")[1][0]["uuid"]
    s2("tag delete", "DELETE", f"/tags/{tid}")
    s2("tag delete again", "DELETE", f"/tags/{tid}")
    # Parts
    s2("applications", "GET", "/applications", d=("id", "uuid", "service_id", "created_at", "updated_at", "last_online_at"))
    s2("databases", "GET", "/databases", d=("id", "uuid", "service_id", "created_at", "updated_at", "last_online_at"))
    apps_a = api("ref", "GET", f"{P}/{ur}/applications")[1]
    apps_b = api("rs", "GET", f"{P}/{urs}/applications")[1]
    if apps_a and apps_b:
        ua, ub = apps_a[0]["uuid"], apps_b[0]["uuid"]
        pd = ("uuid", "service_id", "created_at", "updated_at", "last_online_at")
        for label, method, body in (("app show", "GET", None), ("app extra", "PATCH", {"foo": 1}),
                                    ("app human name", "PATCH", {"human_name": "Friendly", "description": "d"}),
                                    ("app bad url", "PATCH", {"url": "not a url"}),
                                    ("app url", "PATCH", {"url": f"http://svc-{name}.example.test"}),
                                    ("app redirect", "PATCH", {"redirect": "www"}),
                                    ("app drain", "PATCH", {"is_log_drain_enabled": True}),
                                    ("app flags", "PATCH", {"is_gzip_enabled": False, "max_restart_count": 3})):
            bd = body
            if label == "app url":
                # Two services cannot hold the same domain without a conflict: each side gets its own.
                a = api("ref", method, f"{P}/{ur}/applications/{ua}", {"url": f"http://ref-{name}.example.test"})
                b = api("rs", method, f"{P}/{urs}/applications/{ub}", {"url": f"http://rs-{name}.example.test"})
                va = json.loads(json.dumps(sa.value(a[1], pd)).replace("ref-", "x-"))
                vb = json.loads(json.dumps(sb.value(b[1], pd)).replace("rs-", "x-"))
                compare(f"{name}: {label}", [a[0], va], [b[0], vb])
                continue
            a = api("ref", method, f"{P}/{ur}/applications/{ua}", bd)
            b = api("rs", method, f"{P}/{urs}/applications/{ub}", bd)
            va = json.loads(json.dumps(sa.value(a[1], pd)).replace("ref-", "x-"))
            vb = json.loads(json.dumps(sb.value(b[1], pd)).replace("rs-", "x-"))
            compare(f"{name}: {label}", [a[0], va], [b[0], vb])
        a = api("rs", "PATCH", f"{P}/{urs}/applications/{ub}", {"url": f"http://ref-{name}.example.test"})
        b = api("ref", "PATCH", f"{P}/{ur}/applications/{ua}", {"url": f"http://rs-{name}.example.test"})
        compare(f"{name}: url conflict", [a[0], sb.value(a[1], ("uuid", "resource_uuid", "resource_name", "domain", "message"))],
                [b[0], sa.value(b[1], ("uuid", "resource_uuid", "resource_name", "domain", "message"))])
        s2("app missing", "GET", "/applications/nope")
        a = api("ref", "GET", f"{P}/{ur}/envs")
        b = api("rs", "GET", f"{P}/{urs}/envs")
        d = ("uuid", "created_at", "updated_at", "value", "real_value")
        compare(f"{name}: env after url", [a[0], by_key(norm_env(sa.value(a[1], d)))], [b[0], by_key(norm_env(sb.value(b[1], d)))])
        c1 = sql(f"SELECT docker_compose_raw FROM services WHERE uuid='{ur}'")
        c2 = sql(f"SELECT docker_compose_raw FROM services WHERE uuid='{urs}'")
        compare(f"{name}: raw after updateCompose", sa.text(c1), sb.text(c2))
    dbs_a = api("ref", "GET", f"{P}/{ur}/databases")[1]
    dbs_b = api("rs", "GET", f"{P}/{urs}/databases")[1]
    if dbs_a and dbs_b:
        ua, ub = dbs_a[0]["uuid"], dbs_b[0]["uuid"]
        pd = ("uuid", "service_id", "created_at", "updated_at", "last_online_at")
        for label, body in (("db extra", {"foo": 1}), ("db human", {"human_name": "DB", "public_port_timeout": 7200}),
                            ("db public no port", {"is_public": True}), ("db drain", {"is_log_drain_enabled": True})):
            a = api("ref", "PATCH", f"{P}/{ur}/databases/{ua}", body)
            b = api("rs", "PATCH", f"{P}/{urs}/databases/{ub}", body)
            compare(f"{name}: {label}", [a[0], sa.value(a[1], pd)], [b[0], sb.value(b[1], pd)])
        s2("db missing", "GET", "/databases/nope")
    # Storages
    s2("storages", "GET", "/storages", d=("id", "uuid", "resource_id", "created_at", "updated_at", "resource_uuid"))
    st = api("ref", "GET", f"{P}/{ur}/storages")[1]
    stb = api("rs", "GET", f"{P}/{urs}/storages")[1]
    if st["persistent_storages"]:
        x, y = st["persistent_storages"][0]["uuid"], stb["persistent_storages"][0]["uuid"]
        d2 = ("id", "uuid", "resource_id", "created_at", "updated_at")
        a = api("ref", "PATCH", f"{P}/{ur}/storages", {"uuid": x, "type": "persistent", "name": "x"})
        b = api("rs", "PATCH", f"{P}/{urs}/storages", {"uuid": y, "type": "persistent", "name": "x"})
        compare(f"{name}: storage read only", list(a), list(b))
        a = api("ref", "PATCH", f"{P}/{ur}/storages", {"uuid": x, "type": "persistent", "is_preview_suffix_enabled": False})
        b = api("rs", "PATCH", f"{P}/{urs}/storages", {"uuid": y, "type": "persistent", "is_preview_suffix_enabled": False})
        compare(f"{name}: storage suffix", [a[0], sa.value(a[1], d2)], [b[0], sb.value(b[1], d2)])
        a = api("ref", "DELETE", f"{P}/{ur}/storages/{x}")
        b = api("rs", "DELETE", f"{P}/{urs}/storages/{y}")
        compare(f"{name}: storage delete read only", list(a), list(b))
    s2("storage missing", "PATCH", "/storages", {"uuid": "nope", "type": "file"})
    s2("storage no id", "PATCH", "/storages", {"type": "file"})
    s2("storage delete missing", "DELETE", "/storages/nope")
    s2("storage create bad", "POST", "/storages", {"type": "x", "resource_uuid": "r", "mount_path": "/x"})
    s2("storage create no resource", "POST", "/storages", {"type": "file", "resource_uuid": "nope", "mount_path": "/x"})
    # File storages through the API: created, changed and deleted, with read-only rules.
    parts_a = api("ref", "GET", f"{P}/{ur}/applications")[1]
    parts_b = api("rs", "GET", f"{P}/{urs}/applications")[1]
    if parts_a and parts_b:
        ra, rb = parts_a[0]["uuid"], parts_b[0]["uuid"]
        fd = ("id", "uuid", "resource_id", "created_at", "updated_at", "fs_path")
        for label, extra in (("no mount", {"mount_path": ""}), ("dots", {"mount_path": "/a/../b"}), ("host and dir", {"mount_path": "/x", "is_directory": True, "is_host_file": True}),
                             ("dir without fs", {"mount_path": "/x", "is_directory": True}), ("host relative", {"mount_path": "/x", "is_host_file": True, "fs_path": "rel"}),
                             ("host content", {"mount_path": "/x", "is_host_file": True, "fs_path": "/etc/h", "content": "c"}),
                             ("name on file", {"mount_path": "/x", "name": "n"}), ("outside", {"mount_path": "/x", "fs_path": "../../x"})):
            a = api("ref", "POST", f"{P}/{ur}/storages", {"type": "file", "resource_uuid": ra, **extra})
            b = api("rs", "POST", f"{P}/{urs}/storages", {"type": "file", "resource_uuid": rb, **extra})
            compare(f"{name}: file storage {label}", [a[0], sa.value(a[1], fd)], [b[0], sb.value(b[1], fd)])
        a = api("ref", "POST", f"{P}/{ur}/storages", {"type": "persistent", "resource_uuid": ra, "name": "extra", "mount_path": "/extra"})
        b = api("rs", "POST", f"{P}/{urs}/storages", {"type": "persistent", "resource_uuid": rb, "name": "extra", "mount_path": "/extra"})
        pd = ("id", "uuid", "resource_id", "created_at", "updated_at", "name")
        compare(f"{name}: persistent storage create", [a[0], sa.value(a[1], pd)], [b[0], sb.value(b[1], pd)])
        a = api("ref", "POST", f"{P}/{ur}/storages", {"type": "file", "resource_uuid": ra, "mount_path": "/etc/app.conf", "content": "v1"})
        b = api("rs", "POST", f"{P}/{urs}/storages", {"type": "file", "resource_uuid": rb, "mount_path": "/etc/app.conf", "content": "v1"})
        fp = ("id", "uuid", "resource_id", "created_at", "updated_at", "last_online_at", "service_id", "last_saved_proxy_configuration", "sentinel_updated_at")
        compare(f"{name}: file storage create", [a[0], sa.value(a[1], fp)], [b[0], sb.value(b[1], fp)])
        time.sleep(4)
        compare(f"{name}: file written on the server", file_text(ur, "etc/app.conf"), file_text(urs, "etc/app.conf"))
        x, y = a[1].get("uuid"), b[1].get("uuid")
        if x and y:
            a = api("ref", "PATCH", f"{P}/{ur}/storages", {"uuid": x, "type": "file", "content": "v2"})
            b = api("rs", "PATCH", f"{P}/{urs}/storages", {"uuid": y, "type": "file", "content": "v2"})
            compare(f"{name}: file storage update", [a[0], sa.value(a[1], fp)], [b[0], sb.value(b[1], fp)])
            time.sleep(4)
            a = api("ref", "PATCH", f"{P}/{ur}/storages", {"uuid": x, "type": "file", "name": "n"})
            b = api("rs", "PATCH", f"{P}/{urs}/storages", {"uuid": y, "type": "file", "name": "n"})
            compare(f"{name}: file storage name", list(a), list(b))
            a = api("ref", "DELETE", f"{P}/{ur}/storages/{x}")
            b = api("rs", "DELETE", f"{P}/{urs}/storages/{y}")
            compare(f"{name}: file storage delete", list(a), list(b))
            time.sleep(2)
            compare(f"{name}: file removed from the server", file_text(ur, "etc/app.conf"), file_text(urs, "etc/app.conf"))
    # Scheduled tasks
    task = {"name": "t1", "command": "echo hi", "frequency": "daily"}
    td = ("uuid", "created_at", "updated_at")
    s2("task bad cron", "POST", "/scheduled-tasks", {**task, "frequency": "nope"})
    s2("task extra", "POST", "/scheduled-tasks", {**task, "x": 1})
    s2("task create", "POST", "/scheduled-tasks", task, d=td)
    s2("task list", "GET", "/scheduled-tasks", d=td)
    tasks_a, tasks_b = api("ref", "GET", f"{P}/{ur}/scheduled-tasks")[1], api("rs", "GET", f"{P}/{urs}/scheduled-tasks")[1]
    if not (tasks_a and tasks_b):
        record(f"{name}: task rows on both sides", False, f"{tasks_a} | {tasks_b}")
        return
    tu, tv = tasks_a[0]["uuid"], tasks_b[0]["uuid"]
    a = api("ref", "PATCH", f"{P}/{ur}/scheduled-tasks/{tu}", {"enabled": False, "timeout": 60})
    b = api("rs", "PATCH", f"{P}/{urs}/scheduled-tasks/{tv}", {"enabled": False, "timeout": 60})
    compare(f"{name}: task update", [a[0], sa.value(a[1], td)], [b[0], sb.value(b[1], td)])
    a = api("ref", "GET", f"{P}/{ur}/scheduled-tasks/{tu}/executions")
    b = api("rs", "GET", f"{P}/{urs}/scheduled-tasks/{tv}/executions")
    compare(f"{name}: task executions", list(a), list(b))
    a = api("ref", "DELETE", f"{P}/{ur}/scheduled-tasks/{tu}")
    b = api("rs", "DELETE", f"{P}/{urs}/scheduled-tasks/{tv}")
    compare(f"{name}: task delete", list(a), list(b))
    s2("task missing", "PATCH", "/scheduled-tasks/nope", {"enabled": False})
    s2("logs no sub", "GET", "/logs")
    s2("start get", "GET", "/start")


def file_tree(uuid):
    d = f"/data/coolify/services/{uuid}"
    return vm(f"cd {d} 2>/dev/null && find . -mindepth 1 | sort")


def file_text(uuid, name):
    return vm(f"cat /data/coolify/services/{uuid}/{name} 2>/dev/null")


def env_map(text):
    out = {}
    for line in text.splitlines():
        k, _, v = line.partition("=")
        out[k] = shape(v.strip('"')) if GENERATED.match(k) else v
    return out


def wait_status(uuid, wanted, timeout=240):
    end = time.time() + timeout
    last = None
    while time.time() < end:
        last = sql(f"SELECT string_agg(x.status, ',' ORDER BY x.name) FROM (SELECT a.name, a.status FROM service_applications a JOIN services s ON s.id=a.service_id WHERE s.uuid='{uuid}' UNION ALL SELECT d.name, d.status FROM service_databases d JOIN services s ON s.id=d.service_id WHERE s.uuid='{uuid}') x")
        if wanted(last or ""):
            return last
        time.sleep(3)
    return last


def containers(uuid):
    fmt = "{{.Names}}\\t{{.Image}}\\t{{.State}}"
    out = vm(f"docker ps -a --filter label=coolify.serviceUuid={uuid} --format '{fmt}' | sort")
    return out.replace(uuid, "{S}")


def live(name, body, project, http_host=None):
    ua, ub = create_pair(f"live {name}", body, project)
    if not ua:
        return
    sa, sb = Scrub(ua), Scrub(ub)
    try:
        time.sleep(5)
        compare(f"live {name}: files after create", sa.text(file_tree(ua)), sb.text(file_tree(ub)))
        for u, side in ((ua, "ref"), (ub, "rs")):
            print(f"  starting {side}: {api(side, 'POST', f'/api/v1/services/{u}/start')[0]}")
        running = lambda v: v and all("running" in p or "exited" in p for p in v.split(",")) and "running" in v
        ra = wait_status(ua, running)
        rb = wait_status(ub, running)
        compare(f"live {name}: status after start", ra, rb)
        compare(f"live {name}: files", sa.text(file_tree(ua)), sb.text(file_tree(ub)))
        compare(f"live {name}: compose file", sa.text(file_text(ua, "docker-compose.yml")), sb.text(file_text(ub, "docker-compose.yml")))
        ea, eb = env_map(sa.text(file_text(ua, ENV_FILE))), env_map(sb.text(file_text(ub, ENV_FILE)))
        # Upstream reads the variables without ORDER BY, so lines of one rank follow the heap order.
        compare(f"live {name}: env file", dict(sorted(ea.items())), dict(sorted(eb.items())))
        rank = lambda e: [("1" if k.startswith("SERVICE_") else "3") for k in e]
        compare(f"live {name}: env file ranks", rank(ea), rank(eb))
        compare(f"live {name}: containers", containers(ua), containers(ub))
        a = api("ref", "GET", f"/api/v1/services/{ua}")[1]
        b = api("rs", "GET", f"/api/v1/services/{ub}")[1]
        compare(f"live {name}: service status", a.get("status"), b.get("status"))
        if http_host:
            codes = []
            for u in (ua, ub):
                host = sql(f"SELECT a.fqdn FROM service_applications a JOIN services s ON s.id=a.service_id WHERE s.uuid='{u}' AND a.fqdn IS NOT NULL LIMIT 1")
                h = re.sub(r"^https?://", "", host).split(":")[0].split("/")[0]
                codes.append(vm(f"curl -s -o /dev/null -w '%{{http_code}}' --max-time 10 -H 'Host: {h}' http://127.0.0.1/"))
            compare(f"live {name}: http through proxy", codes[0], codes[1])
        # Logs
        part = sql(f"SELECT a.name FROM service_applications a JOIN services s ON s.id=a.service_id WHERE s.uuid='{ua}' ORDER BY a.name LIMIT 1")
        a = api("ref", "GET", f"/api/v1/services/{ua}/logs?sub_service_name={part}&lines=1")
        b = api("rs", "GET", f"/api/v1/services/{ub}/logs?sub_service_name={part}&lines=1")
        compare(f"live {name}: logs status", a[0], b[0])
        a = api("ref", "GET", f"/api/v1/services/{ua}/logs?sub_service_name=nope")
        b = api("rs", "GET", f"/api/v1/services/{ub}/logs?sub_service_name=nope")
        compare(f"live {name}: logs unknown part", list(a), list(b))
        # Stop, start, restart.
        a = api("ref", "POST", f"/api/v1/services/{ua}/start")
        b = api("rs", "POST", f"/api/v1/services/{ub}/start")
        compare(f"live {name}: start when running", list(a), list(b))
        a = api("ref", "POST", f"/api/v1/services/{ua}/stop")
        b = api("rs", "POST", f"/api/v1/services/{ub}/stop")
        compare(f"live {name}: stop", list(a), list(b))
        stopped = lambda v: v and all("exited" in p for p in v.split(","))
        compare(f"live {name}: status after stop", wait_status(ua, stopped), wait_status(ub, stopped))
        compare(f"live {name}: no containers after stop", containers(ua), containers(ub))
        a = api("ref", "POST", f"/api/v1/services/{ua}/stop")
        b = api("rs", "POST", f"/api/v1/services/{ub}/stop")
        compare(f"live {name}: stop when stopped", list(a), list(b))
        a = api("ref", "POST", f"/api/v1/services/{ua}/restart")
        b = api("rs", "POST", f"/api/v1/services/{ub}/restart")
        compare(f"live {name}: restart", list(a), list(b))
        compare(f"live {name}: status after restart", wait_status(ua, running), wait_status(ub, running))
        compare(f"live {name}: containers after restart", containers(ua), containers(ub))
        # A scheduled task through the same container on both sides.
        results_t = []
        for side, u in (("ref", ua), ("rs", ub)):
            tid = api(side, "POST", f"/api/v1/services/{u}/scheduled-tasks", {"name": "t", "command": "/whoami --help 2>&1 | head -1; echo from-task", "frequency": "daily", "container": part})[1]["uuid"]
            st = api(side, "POST", f"/api/v1/services/{u}/scheduled-tasks/{tid}/execute")
            for _ in range(40):
                ex = api(side, "GET", f"/api/v1/services/{u}/scheduled-tasks/{tid}/executions")[1]
                if ex and ex[0].get("finished_at"):
                    break
                time.sleep(3)
            results_t.append((st, [{k: e.get(k) for k in ("status", "message", "retry_count")} for e in ex]))
            api(side, "DELETE", f"/api/v1/services/{u}/scheduled-tasks/{tid}")
        compare(f"live {name}: scheduled task", [list(results_t[0][0]), results_t[0][1]], [list(results_t[1][0]), results_t[1][1]])
        # Part actions.
        pa = sql(f"SELECT a.uuid FROM service_applications a JOIN services s ON s.id=a.service_id WHERE s.uuid='{ua}' ORDER BY a.name LIMIT 1")
        pb = sql(f"SELECT a.uuid FROM service_applications a JOIN services s ON s.id=a.service_id WHERE s.uuid='{ub}' ORDER BY a.name LIMIT 1")
        for action in ("stop", "start", "restart"):
            a = api("ref", "POST", f"/api/v1/services/{ua}/applications/{pa}/{action}")
            b = api("rs", "POST", f"/api/v1/services/{ub}/applications/{pb}/{action}")
            compare(f"live {name}: part {action}", list(a), list(b))
            time.sleep(10)
        a = api("ref", "GET", f"/api/v1/services/{ua}/applications/{pa}/logs?lines=1")
        b = api("rs", "GET", f"/api/v1/services/{ub}/applications/{pb}/logs?lines=1")
        compare(f"live {name}: part logs status", a[0], b[0])
        compare(f"live {name}: compose after part actions", sa.text(file_text(ua, "docker-compose.yml")), sb.text(file_text(ub, "docker-compose.yml")))
    finally:
        vols = {u: vm(f"docker volume ls -q --filter name={u}") for u in (ua, ub)}
        a = api("ref", "DELETE", f"/api/v1/services/{ua}")
        b = api("rs", "DELETE", f"/api/v1/services/{ub}")
        compare(f"live {name}: delete", list(a), list(b))
        gone = wait_gone([ua, ub], 240)
        record(f"live {name}: rows removed", gone, "rows still present")
        for u in (ua, ub):
            record(f"live {name}: nothing left on the VM ({'ref' if u == ua else 'rs'})",
                   not (vm(f"ls -d /data/coolify/services/{u} 2>/dev/null") or vm(f"docker ps -aq --filter label=coolify.serviceUuid={u}")
                        or vm(f"docker volume ls -q --filter name={u}") or vm(f"docker network ls -q --filter name={u}")),
                   f"volumes before delete: {vols[u]}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--all-templates", action="store_true")
    ap.add_argument("--no-live", action="store_true")
    ap.add_argument("--only", default=None)
    ap.add_argument("--custom-only", action="store_true")
    a = ap.parse_args()
    env_uuid = None
    st, proj = api("ref", "POST", "/api/v1/projects", {"name": f"svc-diff-{int(time.time())}"})
    project = proj["uuid"]
    created = []
    try:
        if not a.only:
            errors(project)
            same_api("list as a member", "GET", "/api/v1/services", tok="acme_member_read")
        names = [] if a.custom_only else TEMPLATES
        if a.all_templates:
            names = sorted(json.load(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "../../templates/service-templates-latest.json"))))
        if a.only:
            names = [n for n in names if n == a.only]
        for name in names:
            ua, ub = create_pair(f"template {name}", {"type": name}, project)
            if ua:
                created += [("ref", ua), ("rs", ub)]
                compare_service(f"template {name}", ua, ub, tokens=(ROOT_TOKEN,) if a.all_templates else (ROOT_TOKEN, "acme_member_read"))
                if a.all_templates:
                    remove("ref", ua)
                    remove("rs", ub)
        if not a.only:
            for name, compose in CUSTOM.items():
                ua, ub = create_pair(f"custom {name}", {"docker_compose_raw": b64(compose), "name": f"custom-{name}", "description": "d", "tags": ["diff-tag"]}, project)
                if ua:
                    created += [("ref", ua), ("rs", ub)]
                    compare_service(f"custom {name}", ua, ub)
                    subroutes(name, ua, ub)
                    # Clone: the clone's rows match on both sides.
                    ca = api("ref", "POST", f"/api/v1/services/{ua}/clone", {"destination_uuid": "jjxfu7mlvsrgoxhwrqhrj4yd", "name": "cloned"})
                    cb = api("rs", "POST", f"/api/v1/services/{ub}/clone", {"destination_uuid": "jjxfu7mlvsrgoxhwrqhrj4yd", "name": "cloned"})
                    compare(f"custom {name}: clone", [ca[0], list(ca[1])], [cb[0], list(cb[1])])
                    if ca[0] == 201 and cb[0] == 201:
                        created += [("ref", ca[1]["uuid"]), ("rs", cb[1]["uuid"])]
                        compare_service(f"custom {name} clone", ca[1]["uuid"], cb[1]["uuid"], tokens=(ROOT_TOKEN,))
            # urls on create, instant failures, soft delete
            bad = api("ref", "POST", "/api/v1/services", {"project_uuid": project, "environment_name": "production", "server_uuid": SERVER, "type": "ghost", "urls": [{"name": "nope", "url": "http://x.example.test"}]})
            bad2 = api("rs", "POST", "/api/v1/services", {"project_uuid": project, "environment_name": "production", "server_uuid": SERVER, "type": "ghost", "urls": [{"name": "nope", "url": "http://x.example.test"}]})
            compare("create with an unknown container url", list(bad), list(bad2))
            ua, ub = create_pair("urls", {"type": "uptime-kuma", "urls": [{"name": "uptime-kuma", "url": "http://kuma.example.test"}], "force_domain_override": True}, project)
            if ua:
                created += [("ref", ua), ("rs", ub)]
                compare_service("urls", ua, ub, tokens=(ROOT_TOKEN,))
            # Move.
            st, other = api("ref", "POST", f"/api/v1/projects/{project}/environments", {"name": "other"})
            if st == 201 and created:
                ur, urs = created[0][1], created[1][1]
                a1 = api("ref", "POST", f"/api/v1/services/{ur}/move", {"environment_uuid": other["uuid"]})
                b1 = api("rs", "POST", f"/api/v1/services/{urs}/move", {"environment_uuid": other["uuid"]})
                compare("move", [a1[0], Scrub(ur).value(a1[1])], [b1[0], Scrub(urs).value(b1[1])])
                a1 = api("ref", "POST", f"/api/v1/services/{ur}/move", {"environment_uuid": other["uuid"]})
                b1 = api("rs", "POST", f"/api/v1/services/{urs}/move", {"environment_uuid": other["uuid"]})
                compare("move to the same environment", list(a1), list(b1))
                a1 = api("ref", "POST", f"/api/v1/services/{ur}/move", {"environment_uuid": "nope"})
                b1 = api("rs", "POST", f"/api/v1/services/{urs}/move", {"environment_uuid": "nope"})
                compare("move to an unknown environment", list(a1), list(b1))
                a1 = api("ref", "POST", f"/api/v1/services/{ur}/migrate", {"destination_uuid": "x"})
                b1 = api("rs", "POST", f"/api/v1/services/{urs}/migrate", {"destination_uuid": "x"})
                compare("migrate outside dev", list(a1), list(b1))
        if not a.no_live and not a.only:
            live("custom", {"docker_compose_raw": b64(LIVE_COMPOSE), "name": "live-custom"}, project, http_host=True)
            live("template", {"type": "uptime-kuma"}, project)
    finally:
        for side, u in created:
            remove(side, u)
        wait_gone([u for _, u in created], 300)
        # `$service->delete()` after failed URLs leaves a trashed row on both sides, as upstream does.
        env_ids = sql(f"SELECT string_agg(e.id::text, ',') FROM environments e JOIN projects p ON p.id=e.project_id WHERE p.uuid='{project}'")
        if env_ids:
            for sid in sql(f"SELECT id FROM services WHERE deleted_at IS NOT NULL AND environment_id IN ({env_ids})").splitlines():
                for table, morph in (("service_applications", "ServiceApplication"), ("service_databases", "ServiceDatabase")):
                    for store in ("local_persistent_volumes", "local_file_volumes"):
                        sql(f"DELETE FROM {store} WHERE resource_type='App\\Models\\{morph}' AND resource_id IN (SELECT id FROM {table} WHERE service_id={sid})")
                    sql(f"DELETE FROM {table} WHERE service_id={sid}")
                sql(f"DELETE FROM environment_variables WHERE resourceable_type='App\\Models\\Service' AND resourceable_id={sid}")
                sql(f"DELETE FROM services WHERE id={sid}")
        api("ref", "DELETE", f"/api/v1/projects/{project}")
    print(f"{sum(results)}/{len(results)} checks passed")
    sys.exit(0 if all(results) else 1)


LIVE_COMPOSE = """services:
  whoami:
    image: traefik/whoami:v1.10
    environment:
      - SERVICE_URL_WHOAMI_80
      - SECRET=$SERVICE_PASSWORD_WHO
      - GREETING=${GREETING:-hello}
    volumes:
      - type: bind
        source: ./motd.txt
        target: /motd.txt
        content: "welcome"
      - whodata:/data
  redis:
    image: redis:7-alpine
    command: redis-server --requirepass $SERVICE_PASSWORD_REDIS
    environment:
      - REDIS_PASSWORD=$SERVICE_PASSWORD_REDIS
    volumes:
      - redisdata:/data
volumes:
  whodata:
  redisdata:
"""


if __name__ == "__main__":
    main()
