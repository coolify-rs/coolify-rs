#!/usr/bin/env python3
"""Differential check of the database sub-resources: variables, tags, storages, updates,
move, migrate, clone, SQLite, S3 storages and backup schedules: upstream vs coolify-rs.

    COOLIFY_RS_BASE=http://127.0.0.1:18083 compat/diff/database_resources_diff.py

Each side works on its own database (same request), so the answers are compared with the
UUIDs and dates normalized, and the rows each side writes are diffed. Everything it creates
is deleted at the end. Exit status 1 when anything differs.
"""
import json
import os
import re
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import databases_diff as dd  # noqa: E402

call, sql, record = dd.call, dd.sql, dd.record
SERVER = "hml20fsj5eles0xfiqidq8j5"
ENV_NAME = "production"
ROOT = "acme_owner_root"
SIDES = ("ref", "rs")
ids = {"ref": {}, "rs": {}}

UUID_RE = re.compile(r"\b[a-z0-9]{24}\b")
DATE_RE = re.compile(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?(Z|\+00:00)")
NUM_ID_RE = re.compile(r'"(id|resource_id|resourceable_id|database_id|scheduled_database_backup_id|s3_storage_id|environment_id|destination_id|team_id|server_id|private_key_id|project_id)":\s*\d+')
PORT = re.compile(r"127\.0\.0\.1:\d+")


def norm(side, raw):
    """UUIDs of this side's objects become their names; dates and row ids are masked."""
    text = raw.decode() if isinstance(raw, bytes) else raw
    for other in SIDES:
        for name, value in sorted(ids[other].items(), key=lambda kv: -len(str(kv[1]))):
            text = text.replace(str(value), f"<{name}>")
    text = DATE_RE.sub("<date>", text)
    text = NUM_ID_RE.sub(lambda m: f'"{m.group(1)}":"<id>"', text)
    text = UUID_RE.sub("<uuid>", text)
    text = PORT.sub("host", text)
    return text


def ordered(text):
    """Upstream reads variables in heap order: lists of variables compare by key."""
    try:
        v = json.loads(text)
    except ValueError:
        return text
    if isinstance(v, list) and v and all(isinstance(x, dict) and "key" in x for x in v):
        return json.dumps(sorted(v, key=lambda x: x["key"]))
    # Deliberate divergence (docs/KNOWN_LIMITATIONS.md): upstream's storage answers carry the loaded
    # model relation, with the nested server and its proxy configuration. coolify-rs leaves it out.
    if isinstance(v, dict) and ("fs_path" in v or "mount_path" in v):
        v.pop("resource", None)
        v.pop("service", None)
        return json.dumps(v)
    return text


def fill(side, v):
    if isinstance(v, str):
        return re.sub(r"\{(\w+)\}", lambda m: str(ids[side].get(m.group(1), m.group(0))), v)
    if isinstance(v, list):
        return [fill(side, x) for x in v]
    if isinstance(v, dict):
        return {k: fill(side, x) for k, x in v.items()}
    return v


def both(name, method, path, body=None, tok=ROOT, save=None):
    """The same request on both sides. `path` may hold {name} placeholders of ids[side]."""
    out = {}
    for side in SIDES:
        p = fill(side, path)
        b = fill(side, body)
        status, raw = call(side, method, p, b, tok)
        out[side] = (status, raw)
        if save and status < 300:
            ids[side][save] = json.loads(raw)["uuid"]
    a = (out["ref"][0], ordered(norm("ref", out["ref"][1])))
    b = (out["rs"][0], ordered(norm("rs", out["rs"][1])))
    record(name, a == b, explain(a, b))
    return out


def paths(a, b, at=""):
    """The JSON paths where two values differ; key order counts."""
    if isinstance(a, dict) and isinstance(b, dict):
        out = []
        if list(a) != list(b):
            out.append(f"{at}: keys only-ref {[k for k in a if k not in b]} only-rs {[k for k in b if k not in a]} order-same={set(a) == set(b)}")
        for k in a:
            if k in b:
                out += paths(a[k], b[k], f"{at}.{k}")
        return out
    if isinstance(a, list) and isinstance(b, list):
        out = [] if len(a) == len(b) else [f"{at}: length {len(a)} / {len(b)}"]
        for i, (x, y) in enumerate(zip(a, b)):
            out += paths(x, y, f"{at}[{i}]")
        return out
    return [] if a == b else [f"{at}: {json.dumps(a)[:150]} | {json.dumps(b)[:150]}"]


def explain(a, b):
    try:
        ja, jb = json.loads(a[1]), json.loads(b[1])
    except ValueError:
        return f"ref {a}\n  rs  {b}"
    found = paths(ja, jb)[:12]
    if not found:  # the same values, written differently (0 and 0.0)
        at = next((i for i, (x, y) in enumerate(zip(a[1], b[1])) if x != y), 0)
        found = [f"text at {at}: {a[1][max(0, at - 60):at + 30]} | {b[1][max(0, at - 60):at + 30]}"]
    return f"status {a[0]} / {b[0]}\n  " + "\n  ".join(found)


def row(table, where):
    text = sql(f"SELECT row_to_json(t)::text FROM {table} t WHERE {where} ORDER BY id")
    return [json.loads(x) for x in text.splitlines()]


MASK = {"id", "uuid", "created_at", "updated_at", "resource_id", "resourceable_id", "database_id", "team_id",
        "scheduled_database_backup_id", "s3_storage_id", "value", "key", "secret", "content",
        "environment_id", "destination_id", "last_online_at", "started_at", "config_hash"}


def rows_alike(name, table, where_for):
    data = {}
    for side in SIDES:
        rows = row(table, where_for(side))
        cleaned = []
        for r in rows:
            c = {k: v for k, v in r.items() if k not in MASK and "password" not in k}
            c = json.loads(norm(side, json.dumps(c)))
            cleaned.append(c)
        data[side] = cleaned
    record(name, data["ref"] == data["rs"], "\n  ".join(paths(data["ref"], data["rs"])[:12]))


def main():
    project = {}
    for side in SIDES:
        s, r = call(side, "POST", "/api/v1/projects", {"name": "resources-diff"}, ROOT)
        ids[side]["project"] = json.loads(r)["uuid"]
        project[side] = ids[side]["project"]
        ids[side]["env_prod"] = sql(
            f"SELECT e.uuid FROM environments e JOIN projects p ON p.id=e.project_id WHERE p.uuid='{project[side]}' AND e.name='production'")
        s, r = call(side, "POST", f"/api/v1/projects/{project[side]}/environments", {"name": "other"}, ROOT)
        ids[side]["env_other"] = json.loads(r)["uuid"]
    base = {"server_uuid": SERVER, "project_uuid": "{project}", "environment_name": ENV_NAME}
    destination = sql(f"SELECT sd.uuid FROM standalone_dockers sd JOIN servers s ON s.id=sd.server_id WHERE s.uuid='{SERVER}' ORDER BY sd.id LIMIT 1")
    try:
        # --- SQLite
        both("sqlite create", "POST", "/api/v1/databases/sqlite", dict(base, name="sql-a", sqlite_databases="a.db,b.db"), save="sqlite")
        both("sqlite create bad file", "POST", "/api/v1/databases/sqlite", dict(base, sqlite_databases="../x"))
        both("sqlite refuses public_port", "POST", "/api/v1/databases/sqlite", dict(base, public_port=2000))
        both("sqlite show", "GET", "/api/v1/databases/{sqlite}", tok="acme_owner_root")
        both("sqlite show (read)", "GET", "/api/v1/databases/{sqlite}", tok="acme_owner_read")
        both("sqlite patch", "PATCH", "/api/v1/databases/{sqlite}", {"sqlite_databases": "c.db", "description": "x"})
        both("sqlite patch refuses is_public", "PATCH", "/api/v1/databases/{sqlite}", {"is_public": True})
        rows_alike("sqlite rows", "standalone_sqlites", lambda s: f"uuid='{ids[s]['sqlite']}'")
        both("sqlite backups allowed", "POST", "/api/v1/databases/{sqlite}/backups", {"frequency": "daily"}, save="sqlite_backup")

        # --- a PostgreSQL to work on
        both("create pg with tags", "POST", "/api/v1/databases/postgresql", dict(base, name="pg-a", tags=["Foo", "bar"], postgres_password="Pass-1"), save="pg")
        both("tags after create", "GET", "/api/v1/databases/{pg}/tags")
        both("create redis", "POST", "/api/v1/databases/redis", dict(base, name="r-a", redis_password="Redis-1"), save="redis")
        both("create dragonfly", "POST", "/api/v1/databases/dragonfly", dict(base, name="d-a"), save="dragonfly")

        # --- PATCH
        P = "/api/v1/databases/{pg}"
        for i, b in enumerate([
            {"foo": 1},
            {"name": 5},
            {"public_port": 80},
            {"public_port_timeout": 0},
            {"limits_memory": 5},
            {"postgres_user": "1bad"},
            {"postgres_conf": "not base64!"},
            {"health_check_interval": 0},
            {"instant_deploy": "x"},
            {"is_log_drain_enabled": True},
            {"redact_all_env_values_in_logs": "x"},
            {"redis_conf": "x"},
        ]):
            both(f"patch error {i}", "PATCH", P, b)
        both("patch bad json", "PATCH", P, None)
        both("patch fields", "PATCH", P, {"name": "pg <b>b</b>", "description": "de", "limits_memory": "256m", "limits_cpus": "0.5",
                                          "postgres_conf": dd.b64("max_connections = 50"), "health_check_interval": 7,
                                          "postgres_initdb_args": "--data-checksums", "public_port_timeout": 60})
        rows_alike("patched rows", "standalone_postgresqls", lambda s: f"uuid='{ids[s]['pg']}'")
        both("patch toggles", "PATCH", P, {"redact_all_env_values_in_logs": False, "include_coolify_default_variables": False})
        rows_alike("toggled rows", "standalone_postgresqls", lambda s: f"uuid='{ids[s]['pg']}'")
        both("patch without write", "PATCH", P, {"name": "x"}, tok="acme_owner_read")
        both("patch member", "PATCH", P, {"name": "x"}, tok="acme_member_write_legacy")
        both("patch is_public without deploy", "PATCH", P, {"is_public": True, "public_port": 45001}, tok="acme_owner_write")
        both("patch instant without deploy", "PATCH", P, {"name": "pg", "instant_deploy": True}, tok="acme_owner_write")
        both("patch missing", "PATCH", "/api/v1/databases/nope", {"name": "x"})
        both("patch redis password", "PATCH", "/api/v1/databases/{redis}", {"redis_password": "Redis-2"})
        both("redis show after password", "GET", "/api/v1/databases/{redis}")
        both("patch dragonfly password", "PATCH", "/api/v1/databases/{dragonfly}", {"dragonfly_password": "Drag-2"})
        both("dragonfly show", "GET", "/api/v1/databases/{dragonfly}")

        # --- variables
        E = "/api/v1/databases/{pg}/envs"
        both("envs empty", "GET", E)
        for i, b in enumerate([{}, {"key": "1bad"}, {"key": "A B"}, {"key": "OK", "is_literal": "x"}, {"key": "OK", "comment": "x" * 300}, {"key": "OK", "value": 5}]):
            both(f"env error {i}", "POST", E, b)
        both("env create", "POST", E, {"key": "FOO", "value": " bar ", "is_literal": True, "comment": "c"}, save="env_foo")
        both("env create plain", "POST", E, {"key": "BAZ"}, save="env_baz")
        both("env conflict", "POST", E, {"key": "FOO", "value": "x"})
        both("env create shown once", "POST", E, {"key": "ONCE", "value": "secret-value", "is_shown_once": True})
        both("env shared reference", "POST", E, {"key": "SHARED", "value": "{{team.NOPE}}"})
        both("env list (root)", "GET", E)
        both("env list (read)", "GET", E, tok="acme_owner_read")
        both("env update", "PATCH", E, {"key": "FOO", "value": "new", "is_literal": False, "is_multiline": True, "comment": None})
        both("env update missing", "PATCH", E, {"key": "NOPE", "value": "x"})
        both("env bulk none", "PATCH", E + "/bulk", {"foo": 1})
        both("env bulk bad item", "PATCH", E + "/bulk", {"data": [{"key": "1bad"}]})
        both("env bulk", "PATCH", E + "/bulk", {"data": [{"key": "BULK1", "value": "1"}, {"key": "FOO", "value": "again"}, {"key": "BULK2"}]})
        both("env list after bulk", "GET", E)
        both("env delete missing", "DELETE", E + "/nope")
        both("env delete", "DELETE", E + "/{env_baz}")
        both("env without write", "POST", E, {"key": "X"}, tok="acme_owner_read")
        both("env member", "POST", E, {"key": "X"}, tok="acme_member_write_legacy")
        both("env member list", "GET", E, tok="acme_member_read")
        both("env missing db", "GET", "/api/v1/databases/nope/envs")
        data = {}
        for side in SIDES:
            rid = sql(f"SELECT id FROM standalone_postgresqls WHERE uuid='{ids[side]['pg']}'")
            data[side] = [{k: v for k, v in r.items() if k not in MASK} for r in row("environment_variables", f"resourceable_type='App\\Models\\StandalonePostgresql' AND resourceable_id={rid}")]
        record("variable rows", data["ref"] == data["rs"], f"{data['ref']}\n  {data['rs']}")

        # --- tags
        T = "/api/v1/databases/{pg}/tags"
        both("tags list", "GET", T)
        for i, b in enumerate([{"foo": 1}, {"tag_name": "x"}, {"tag_name": 5}, {"tag_names": []}, {"tag_names": ["ok", 1]}, {"tag_name": "ok", "tag_names": ["a"]}, {"tag_name": "ok", "x": 1}]):
            both(f"tag error {i}", "POST", T, b)
        both("tag add", "POST", T, {"tag_name": " Baz<b> "})
        both("tag add many", "POST", T, {"tag_names": ["foo", "NEWT"]})
        for side in SIDES:
            ids[side]["tag"] = sql(f"SELECT uuid FROM tags WHERE name='baz' AND team_id=4 LIMIT 1")
        both("tag delete", "DELETE", T + "/{tag}")
        both("tag delete again", "DELETE", T + "/{tag}")
        both("tag delete unknown", "DELETE", T + "/nope")
        both("tag list after", "GET", T)
        both("tag add read token", "POST", T, {"tag_name": "ok"}, tok="acme_owner_read")

        # --- storages
        S = "/api/v1/databases/{pg}/storages"
        both("storages list", "GET", S)
        for i, b in enumerate([
            {}, {"type": "x", "mount_path": "/a"}, {"type": "persistent"}, {"type": "persistent", "mount_path": "/a"},
            {"type": "persistent", "name": "a b", "mount_path": "/a"}, {"type": "persistent", "name": "a", "mount_path": "/a", "content": "x"},
            {"type": "file", "name": "a", "mount_path": "/a"}, {"type": "file", "mount_path": "/a/../b"}, {"type": "file", "mount_path": "/a", "foo": 1},
            {"type": "file", "mount_path": "/a", "is_directory": True}, {"type": "file", "mount_path": "/a", "is_host_file": True},
            {"type": "file", "mount_path": "/a", "is_directory": True, "is_host_file": True},
            {"type": "file", "mount_path": "/a", "is_host_file": True, "fs_path": "/etc/hosts", "content": "x"},
            {"type": "file", "mount_path": "/a", "is_host_file": True, "fs_path": "relative"},
            {"type": "file", "mount_path": "/a", "fs_path": "../x"},
            {"type": "file", "mount_path": "/a;b"},
            {"type": "file", "mount_path": "/a", "fs_path": "/etc/x"},
        ]):
            both(f"storage error {i}", "POST", S, b, tok="acme_owner_write")
        both("storage content needs deploy", "POST", S, {"type": "file", "mount_path": "/conf/a", "content": "x"}, tok="acme_owner_write")
        both("storage persistent", "POST", S, {"type": "persistent", "name": "extra", "mount_path": "data2"}, save="st_p")
        both("storage host file", "POST", S, {"type": "file", "mount_path": "/h", "is_host_file": True, "fs_path": "/etc/hostname"}, save="st_h")
        both("storage file", "POST", S, {"type": "file", "mount_path": "/conf/a.txt", "content": "hello"}, save="st_f")
        both("storage dir", "POST", S, {"type": "file", "mount_path": "/conf/d", "is_directory": True, "fs_path": "dir1"}, save="st_d")
        both("storages list after", "GET", S)
        both("storages list read", "GET", S, tok="acme_owner_read")
        time.sleep(3)
        data = {}
        for side in SIDES:
            rid = sql(f"SELECT id FROM standalone_postgresqls WHERE uuid='{ids[side]['pg']}'")
            for table in ("local_file_volumes", "local_persistent_volumes"):
                rows = row(table, f"resource_type='App\\Models\\StandalonePostgresql' AND resource_id={rid}")
                data.setdefault(table, {})[side] = [json.loads(norm(side, json.dumps({k: v for k, v in r.items() if k not in MASK}))) for r in rows]
        for table in data:
            record(f"{table} rows", data[table]["ref"] == data[table]["rs"], f"{data[table]['ref']}\n  {data[table]['rs']}")
        for i, b in enumerate([{}, {"type": "file"}, {"type": "file", "uuid": "nope"}, {"type": "persistent", "uuid": "{st_p}", "content": "x"}, {"type": "file", "uuid": "{st_f}", "name": "x"}, {"type": "file", "uuid": "{st_f}", "foo": 1}]):
            both(f"storage update error {i}", "PATCH", S, b)
        both("storage update persistent", "PATCH", S, {"type": "persistent", "uuid": "{st_p}", "name": "extra2", "mount_path": "data3", "is_preview_suffix_enabled": False})
        both("storage update file", "PATCH", S, {"type": "file", "uuid": "{st_f}", "content": "hello2", "mount_path": "/conf/b.txt"})
        both("storage update needs deploy", "PATCH", S, {"type": "file", "uuid": "{st_f}", "content": "hello3"}, tok="acme_owner_write")
        both("storage delete unknown", "DELETE", S + "/nope")
        time.sleep(2)
        both("storage delete file", "DELETE", S + "/{st_f}")
        both("storage delete dir", "DELETE", S + "/{st_d}")
        both("storage delete host", "DELETE", S + "/{st_h}")
        both("storage delete persistent", "DELETE", S + "/{st_p}")

        # --- move, migrate
        M = "/api/v1/databases/{pg}/move"
        for i, b in enumerate([{}, {"environment_uuid": 5}, {"environment_uuid": "nope"}, {"environment_uuid": "{env_other}", "x": 1}]):
            both(f"move error {i}", "POST", M, b)
        both("move same", "POST", M, {"environment_uuid": "{env_prod}"})
        both("move", "POST", M, {"environment_uuid": "{env_other}"})
        both("move read token", "POST", M, {"environment_uuid": "{env_prod}"}, tok="acme_owner_read")
        both("migrate", "POST", "/api/v1/databases/{pg}/migrate", {"destination_uuid": destination})
        both("migrate missing", "POST", "/api/v1/databases/nope/migrate", {"destination_uuid": destination})

        # --- S3 storages
        S3 = "/api/v1/s3-storages"
        both("s3 list", "GET", S3)
        s3base = {"name": "minio test", "description": "d", "endpoint": "https://s3.amazonaws.com", "bucket": "b-ucket", "region": "us-east-1", "key": "k", "secret": "s"}
        for i, b in enumerate([{}, dict(s3base, foo=1), dict(s3base, bucket="a"), dict(s3base, bucket="a..b"), dict(s3base, endpoint="ftp://x"),
                               dict(s3base, endpoint="http://127.0.0.1:9000"), dict(s3base, endpoint="http://169.254.169.254"), dict(s3base, endpoint="http://10.1.1.1"),
                               dict(s3base, endpoint="nope"), dict(s3base, name="a"), dict(s3base, name="<bad>"), dict(s3base, region=None), dict(s3base, is_usable="x")]):
            both(f"s3 error {i}", "POST", S3, b)
        both("s3 create member", "POST", S3, s3base, tok="acme_member_write_legacy")
        both("s3 create read", "POST", S3, s3base, tok="acme_owner_read")
        both("s3 create", "POST", S3, s3base, save="s3a")
        both("s3 create second", "POST", S3, dict(s3base, name="minio two", is_usable=True), save="s3b")
        both("s3 list root", "GET", S3)
        both("s3 list read", "GET", S3, tok="acme_owner_read")
        both("s3 show", "GET", S3 + "/{s3a}")
        both("s3 show read", "GET", S3 + "/{s3a}", tok="acme_owner_read")
        both("s3 show missing", "GET", S3 + "/nope")
        both("s3 patch", "PATCH", S3 + "/{s3a}", {"description": "e", "key": "  k2 ", "is_usable": True})
        both("s3 patch bad", "PATCH", S3 + "/{s3a}", {"bucket": "a"})
        both("s3 patch missing", "PATCH", S3 + "/nope", {"region": "x"})
        both("s3 show after patch", "GET", S3 + "/{s3a}")
        rows_alike("s3 rows", "s3_storages", lambda s: f"uuid='{ids[s]['s3a']}'")
        both("s3 validate missing", "POST", S3 + "/nope/validate", {})

        # --- backup schedules
        B = "/api/v1/databases/{pg}/backups"
        both("backups empty", "GET", B)
        for i, b in enumerate([
            {}, {"frequency": "nope nope"}, {"frequency": "daily", "foo": 1}, {"frequency": "daily", "save_s3": True},
            {"frequency": "daily", "disable_local_backup": True}, {"frequency": "daily", "s3_storage_uuid": "nope"},
            {"frequency": "daily", "s3_storage_uuids": ["nope"]}, {"frequency": "daily", "s3_storage_uuids": "x"},
            {"frequency": "daily", "s3_storage_uuids": ["a", "a"]}, {"frequency": "daily", "timeout": 5}, {"frequency": "daily", "timeout": 99999},
            {"frequency": "daily", "database_backup_retention_amount_locally": -1}, {"frequency": "daily", "databases_to_backup": "a;b"},
            {"frequency": "daily", "missing_backup_notification_days": 400}, {"frequency": "daily", "enabled": "x"},
        ]):
            both(f"backup error {i}", "POST", B, b)
        both("backup now without deploy", "POST", B, {"frequency": "daily", "backup_now": True}, tok="acme_owner_write")
        both("backup member", "POST", B, {"frequency": "daily"}, tok="acme_member_write_legacy")
        both("backup on redis", "POST", "/api/v1/databases/{redis}/backups", {"frequency": "daily"})
        both("backup create", "POST", B, {"frequency": "0 2 * * *", "database_backup_retention_amount_locally": 3, "database_backup_retention_max_storage_locally": 1.5, "timeout": 120, "dump_all": False}, save="backup")
        both("backup create s3", "POST", B, {"frequency": "@daily", "save_s3": True, "s3_storage_uuids": ["{s3b}", "{s3a}"], "s3_storage_uuid": "{s3a}", "disable_local_backup": True}, save="backup2")
        both("backup uuid not in list", "POST", B, {"frequency": "daily", "s3_storage_uuids": ["{s3a}"], "s3_storage_uuid": "{s3b}"})
        both("backups list", "GET", B)
        both("backups list read", "GET", B, tok="acme_owner_read")
        rows_alike("backup rows", "scheduled_database_backups", lambda s: f"uuid IN ('{ids[s]['backup']}','{ids[s]['backup2']}')")
        U = B + "/{backup}"
        for i, b in enumerate([{"foo": 1}, {"frequency": "bad bad"}, {"save_s3": True}, {"disable_local_backup": True}, {"s3_storage_uuid": "nope"}, {"timeout": 5}, {"databases_to_backup": "a|b;c"}]):
            both(f"backup update error {i}", "PATCH", U, b)
        both("backup update missing", "PATCH", B + "/nope", {"enabled": False})
        both("backup update", "PATCH", U, {"enabled": False, "frequency": "hourly", "databases_to_backup": "x,y", "database_backup_retention_days_locally": 2})
        both("backup update s3", "PATCH", U, {"save_s3": True, "s3_storage_uuid": "{s3a}"})
        both("backup update s3 again", "PATCH", B + "/{backup2}", {"s3_storage_uuid": "{s3b}"})
        both("backups after update", "GET", B)
        rows_alike("backup rows after update", "scheduled_database_backups", lambda s: f"uuid IN ('{ids[s]['backup']}','{ids[s]['backup2']}')")
        both("list databases shows configs", "GET", "/api/v1/databases/{pg}")
        both("executions", "GET", U + "/executions")
        both("executions missing", "GET", B + "/nope/executions")
        both("execution delete missing", "DELETE", U + "/executions/nope")
        both("s3 delete moves schedules", "DELETE", S3 + "/{s3a}")
        both("backups after s3 delete", "GET", B)
        rows_alike("backup rows after s3 delete", "scheduled_database_backups", lambda s: f"uuid IN ('{ids[s]['backup']}','{ids[s]['backup2']}')")
        both("backup delete missing", "DELETE", B + "/nope")
        both("backup delete", "DELETE", B + "/{backup2}")
        both("s3 delete", "DELETE", S3 + "/{s3b}")

        # --- clone (no volumes), the source has a schedule, variables and tags
        C = "/api/v1/databases/{pg}/clone"
        for i, b in enumerate([{}, {"destination_uuid": "nope"}, {"destination_uuid": destination, "x": 1}, {"destination_uuid": destination, "name": 5}]):
            both(f"clone error {i}", "POST", C, b)
        both("clone volumes without deploy", "POST", C, {"destination_uuid": destination, "clone_volumes": True}, tok="acme_owner_write")
        both("clone", "POST", C, {"destination_uuid": destination, "name": "pg-clone"}, save="pg_clone")
        both("clone show", "GET", "/api/v1/databases/{pg_clone}")
        both("clone envs", "GET", "/api/v1/databases/{pg_clone}/envs")
        both("clone tags", "GET", "/api/v1/databases/{pg_clone}/tags")
        both("clone storages", "GET", "/api/v1/databases/{pg_clone}/storages")
        both("clone backups", "GET", "/api/v1/databases/{pg_clone}/backups")
        both("list databases", "GET", "/api/v1/databases")

        # --- logs and imports without a running database
        both("logs read token", "GET", "/api/v1/databases/{pg}/logs", tok="acme_owner_read")
        both("logs not running", "GET", "/api/v1/databases/{pg}/logs")
        I = "/api/v1/databases/{pg}/imports"
        for i, b in enumerate([{}, {"source": "x"}, {"source": "upload"}, {"source": "s3"}, {"source": "server"},
                               {"source": "server", "path": "/a", "upload_id": "x"}, {"source": "upload", "upload_id": "nope"},
                               {"source": "server", "path": "/a", "foo": 1}, {"source": "server", "path": "/a", "dump_all": "x"}]):
            both(f"import error {i}", "POST", I, b)
        both("import without deploy", "POST", I, {"source": "server", "path": "/a"}, tok="acme_owner_write")
        both("import not running", "POST", I, {"source": "server", "path": "/a"})
        both("import redis unsupported", "POST", "/api/v1/databases/{redis}/imports", {"source": "server", "path": "/a"})
        both("import status missing", "GET", I + "/1")
    finally:
        for side in SIDES:
            for key in ("pg", "pg_clone", "redis", "dragonfly", "sqlite"):
                if key in ids[side]:
                    call(side, "DELETE", f"/api/v1/databases/{ids[side][key]}", tok=ROOT)
        time.sleep(8)
        for side in SIDES:
            for key in ("s3a", "s3b"):
                if key in ids[side]:
                    call(side, "DELETE", f"/api/v1/s3-storages/{ids[side][key]}", tok=ROOT)
            if "project" in ids[side]:
                call(side, "DELETE", f"/api/v1/projects/{ids[side]['project']}", tok=ROOT)
    passed = sum(dd.results)
    print(f"\n{passed}/{len(dd.results)} checks identical")
    sys.exit(0 if passed == len(dd.results) else 1)


if __name__ == "__main__":
    main()
