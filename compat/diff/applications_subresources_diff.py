#!/usr/bin/env python3
"""Differential check of the application sub-resources: envs, storages, tags, secret manager.

Each side gets its own application, created through its own API, and every request goes to both.
Responses are compared with key order kept, after replacing uuids, ids and timestamps; rows are
compared too, and each side also reads the rows the other wrote (same database). File storages are
compared on the VM. Everything created is deleted at the end. Usage:

    COOLIFY_RS_BASE=http://127.0.0.1:18082 applications_subresources_diff.py \
        --server hml20fsj5eles0xfiqidq8j5 --vm coolify-rs-e2e

Exit status 1 when anything differs.
"""
import argparse
import json
import os
import re
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from databases_diff import call as raw_call, record, results, sql  # noqa: E402


def call(*args, **kwargs):
    """`databases_diff.call`, patient with the API rate limit: this script sends many requests."""
    for _ in range(6):
        try:
            return raw_call(*args, **kwargs)
        except SystemExit:
            time.sleep(30)
    raise SystemExit("rate limited")



ROOT_TOK = "acme_owner_root"
WRITE = "acme_owner_write"
READ = "acme_owner_read"
MEMBER_READ = "acme_member_read"
MEMBER_WRITE = "acme_member_write_legacy"
OTHER = "carol_root"
SIDES = ("ref", "rs")
TIME = re.compile(r"^\d{4}-\d\d-\d\d[T ]\d\d:\d\d:\d\d")
UUIDISH = {"uuid", "env_uuid", "storage_uuid"}
ctx = {"ref": {}, "rs": {}}


def fill(side, v):
    if isinstance(v, str):
        for k, x in ctx[side].items():
            v = v.replace(f"<{k}>", str(x))
        return v
    if isinstance(v, list):
        return [fill(side, x) for x in v]
    if isinstance(v, dict):
        return {k: fill(side, x) for k, x in v.items()}
    return v


def norm(side, v):
    """Replaces what legitimately differs, keeping key order."""
    if isinstance(v, dict):
        out = {}
        for k, x in v.items():
            if k in UUIDISH and x:
                out[k] = "UUID"
            elif k in ("id", "resource_id", "resourceable_id", "taggable_id") and x is not None:
                out[k] = "ID"
            elif k == "config_hash" and x:
                out[k] = "HASH"
            elif k in ("created_at", "updated_at") and x:
                out[k] = "TIME" if TIME.match(str(x)) else f"BAD {x}"
            else:
                out[k] = norm(side, x)
        return out
    if isinstance(v, list):
        return [norm(side, x) for x in v]
    if isinstance(v, str):
        for k, x in ctx[side].items():
            if len(str(x)) > 8:
                v = v.replace(str(x), f"<{k}>")
        return v
    return v


def parsed(raw):
    try:
        return json.loads(raw)
    except ValueError:
        return raw.decode(errors="replace")


def both(name, method, path, body=None, tok=ROOT_TOK, status_only=False):
    """Sends the request to each side; returns {side: parsed body}."""
    out, cmp = {}, {}
    for side in SIDES:
        status, raw = call(side, method, "/api/v1" + fill(side, path), fill(side, body), tok)
        data = parsed(raw)
        out[side] = data
        text = json.dumps(norm(side, data))
        # A generated domain contains the uuid of the new application.
        if isinstance(data, dict) and isinstance(data.get("uuid"), str):
            text = text.replace(data["uuid"], "NEWAPP")
        cmp[side] = (status, None) if status_only else (status, text)
    ok = cmp["ref"] == cmp["rs"]
    a, b = str(cmp["ref"]), str(cmp["rs"])
    at = next((i for i, (x, y) in enumerate(zip(a, b)) if x != y), min(len(a), len(b)))
    record(name, ok, f"first difference at {at}\n  ref ..{a[max(0, at - 80):at + 200]}\n  rs  ..{b[max(0, at - 80):at + 200]}")
    return out


def rows(query, *drop):
    """The rows each side's application has, as JSON without the given columns."""
    out = {}
    for side in SIDES:
        text = sql(fill(side, f"SELECT coalesce(json_agg(row_to_json(x)), '[]')::text FROM ({query}) x"))
        items = json.loads(text)
        for i in items:
            for d in drop:
                i.pop(d, None)
        out[side] = norm(side, items)
    return out


ENV_DROP = ("id", "uuid", "value", "created_at", "updated_at", "resourceable_id")
ENV_ROWS = "SELECT * FROM environment_variables WHERE resourceable_type='App\\Models\\Application' AND resourceable_id=<app_id> ORDER BY id"
STORAGE_DROP = ("id", "uuid", "created_at", "updated_at", "resource_id", "content")


def same_rows(name, query, *drop):
    r = rows(query, *drop)
    record(name, r["ref"] == r["rs"], f"ref {r['ref']}\n  rs  {r['rs']}")


def cross_read(name, path, tok=ROOT_TOK):
    """Each implementation reads what the other wrote: same answer for the same application."""
    for owner in SIDES:
        path_o = fill(owner, path)
        a, b = call("ref", "GET", "/api/v1" + path_o, None, tok), call("rs", "GET", "/api/v1" + path_o, None, tok)
        record(f"{name} (rows of {owner})", a == b, f"ref {a}\n  rs  {b}")


def tinker(code):
    return subprocess.run(["docker", "exec", "coolify-rs-reference-coolify-1", "php", "/var/www/html/artisan", "tinker", "--execute", code],
                          check=True, capture_output=True, text=True).stdout.strip()


def env_cases():
    missing = "does-not-exist"
    both("envs list unknown app", "GET", f"/applications/{missing}/envs")
    both("envs create unknown app", "POST", f"/applications/{missing}/envs", {"key": "A"})
    both("envs patch unknown app", "PATCH", f"/applications/{missing}/envs", {"key": "A"})
    both("envs bulk unknown app", "PATCH", f"/applications/{missing}/envs/bulk", {"data": [{"key": "A"}]})
    both("envs delete unknown app", "DELETE", f"/applications/{missing}/envs/x")
    both("envs other team", "GET", "/applications/<app>/envs", tok=OTHER)
    both("envs create read token", "POST", "/applications/<app>/envs", {"key": "A"}, tok=READ)
    both("envs create member", "POST", "/applications/<app>/envs", {"key": "A"}, tok=MEMBER_WRITE)
    both("envs patch member", "PATCH", "/applications/<app>/envs", {"key": "A"}, tok=MEMBER_WRITE)
    both("envs bulk member", "PATCH", "/applications/<app>/envs/bulk", {"data": [{"key": "A"}]}, tok=MEMBER_WRITE)
    both("envs delete member", "DELETE", "/applications/<app>/envs/x", tok=MEMBER_WRITE)
    both("envs list empty", "GET", "/applications/<app>/envs")
    for label, body in [
        ("no key", {"value": "x"}), ("bad key", {"key": "1bad"}), ("long key", {"key": "A" * 300}),
        ("space key", {"key": "A B"}), ("null key", {"key": None}), ("number key", {"key": 12}), ("true key", {"key": True}),
        ("extra", {"key": "A", "foo": 1}), ("int value", {"key": "A", "value": 1}),
        ("bad bool", {"key": "A", "is_preview": "x", "is_literal": 2, "is_multiline": None}),
        ("long comment", {"key": "A", "comment": "c" * 300}), ("int comment", {"key": "A", "comment": 3}),
        ("many", {"key": "1", "value": [], "is_shown_once": "yes", "bar": 1, "baz": 2}),
    ]:
        both(f"envs create invalid: {label}", "POST", "/applications/<app>/envs", body)
    both("envs create array key", "POST", "/applications/<app>/envs", {"key": ["A"]}, status_only=True)
    both("envs create plain", "POST", "/applications/<app>/envs", {"key": "PLAIN", "value": "  hello  "})
    same_rows("rows after create with preview copy", ENV_ROWS, *ENV_DROP)
    both("envs create conflict", "POST", "/applications/<app>/envs", {"key": "PLAIN", "value": "x"})
    both("envs create all flags", "POST", "/applications/<app>/envs",
         {"key": " FLAGS.A ", "value": "v", "is_literal": True, "is_multiline": True, "is_runtime": False, "is_buildtime": False, "comment": "note"})
    both("envs create numeric bools", "POST", "/applications/<app>/envs",
         {"key": "NUMERIC", "value": "v", "is_literal": 1, "is_multiline": "1", "is_shown_once": 0, "is_runtime": "0"})
    both("envs create shown once", "POST", "/applications/<app>/envs", {"key": "ONCE", "value": "secret", "is_shown_once": True})
    both("envs create preview only", "POST", "/applications/<app>/envs", {"key": "PREVIEW_ONLY", "value": "p", "is_preview": True})
    both("envs create preview conflict", "POST", "/applications/<app>/envs", {"key": "PREVIEW_ONLY", "value": "p", "is_preview": True})
    both("envs create after preview exists", "POST", "/applications/<app>/envs", {"key": "PREVIEW_ONLY", "value": "main"})
    both("envs create null value", "POST", "/applications/<app>/envs", {"key": "NULL_VALUE"})
    both("envs create empty value", "POST", "/applications/<app>/envs", {"key": "EMPTY_VALUE", "value": ""})
    both("envs create control keys", "POST", "/applications/<app>/envs", {"key": "NIXPACKS_X", "value": "1"})
    both("envs create service key", "POST", "/applications/<app>/envs", {"key": "SERVICE_FQDN_X", "value": "1"})
    both("envs create shared ref", "POST", "/applications/<app>/envs", {"key": "SHARED", "value": "{{team.SUBDIFF_SHARED}} and {{ project.NONE }} {{other.X}}"})
    both("envs create pure shared ref", "POST", "/applications/<app>/envs", {"key": "SHARED_PURE", "value": "{{team.SUBDIFF_SHARED}}"})
    both("envs create locked ref", "POST", "/applications/<app>/envs", {"key": "SHARED_LOCKED", "value": "{{team.SUBDIFF_LOCKED}}"})
    both("envs create json", "POST", "/applications/<app>/envs", {"key": "JSON_V", "value": "{\"a\": \"b'c\"}"})
    same_rows("rows after creates", ENV_ROWS, *ENV_DROP)
    for tok in (ROOT_TOK, WRITE, READ, MEMBER_READ):
        both(f"envs list as {tok}", "GET", "/applications/<app>/envs", tok=tok)
    cross_read("envs list", "/applications/<app>/envs")

    both("envs patch invalid json type", "PATCH", "/applications/<app>/envs", {"key": "PLAIN", "foo": 1})
    both("envs patch no key", "PATCH", "/applications/<app>/envs", {"value": "x"})
    both("envs patch not found", "PATCH", "/applications/<app>/envs", {"key": "NOPE", "value": "x"})
    both("envs patch empty", "PATCH", "/applications/<app>/envs", {})
    both("envs patch value", "PATCH", "/applications/<app>/envs", {"key": "PLAIN", "value": "changed"})
    both("envs patch read-only token sees no value", "PATCH", "/applications/<app>/envs", {"key": "PLAIN", "value": "changed2"}, tok=WRITE)
    both("envs patch without value clears it", "PATCH", "/applications/<app>/envs", {"key": "FLAGS.A", "comment": "c2"})
    both("envs patch raw bools", "PATCH", "/applications/<app>/envs",
         {"key": "NUMERIC", "value": "v", "is_literal": 0, "is_shown_once": "0", "is_multiline": 0, "is_runtime": 1, "is_buildtime": "0"})
    both("envs patch preview", "PATCH", "/applications/<app>/envs", {"key": "PLAIN", "value": "pv", "is_preview": True, "is_literal": True})
    both("envs patch to shared", "PATCH", "/applications/<app>/envs", {"key": "EMPTY_VALUE", "value": "{{team.SUBDIFF_SHARED}}"})
    both("envs patch shown once", "PATCH", "/applications/<app>/envs", {"key": "ONCE", "value": "other"})
    same_rows("rows after patches", ENV_ROWS, *ENV_DROP)

    # An unchanged value is not written: the ciphertext and updated_at stay.
    for side in SIDES:
        q = fill(side, "SELECT value FROM environment_variables WHERE resourceable_id=<app_id> AND resourceable_type='App\\Models\\Application' AND key='NUMERIC' AND is_preview=false")
        before = sql(q)
        status, _ = call(side, "PATCH", "/api/v1" + fill(side, "/applications/<app>/envs"), {"key": "NUMERIC", "value": "v"}, ROOT_TOK)
        record(f"unchanged value keeps the ciphertext ({side})", status == 201 and sql(q) == before, f"status {status}")

    both("envs bulk no data", "PATCH", "/applications/<app>/envs/bulk", {"x": 1})
    both("envs bulk empty data", "PATCH", "/applications/<app>/envs/bulk", {"data": []})
    both("envs bulk invalid", "PATCH", "/applications/<app>/envs/bulk", {"data": [{"key": "1bad"}]})
    both("envs bulk scalar", "PATCH", "/applications/<app>/envs/bulk", {"data": "x"})
    both("envs bulk partly invalid", "PATCH", "/applications/<app>/envs/bulk", {"data": [{"key": "BULK_FIRST", "value": "1"}, {"key": "bad key"}]})
    both("envs bulk upsert", "PATCH", "/applications/<app>/envs/bulk", {"data": [
        {"key": "BULK_NEW", "value": "n", "is_literal": 1, "is_shown_once": False, "comment": "c", "foo": "dropped"},
        {"key": "BULK_NEW2", "value": "{{team.SUBDIFF_SHARED}}", "is_runtime": False},
        {"key": "BULK_PREVIEW", "value": "p", "is_preview": True},
        {"key": "PLAIN", "value": "bulk"},
        {"key": "PLAIN", "is_preview": True, "value": "bulk-p", "is_multiline": "1"},
        {"key": "BULK_ONCE", "value": "o", "is_shown_once": 1},
    ]})
    both("envs bulk keyed data", "PATCH", "/applications/<app>/envs/bulk", {"data": {"a": {"key": "BULK_KEYED", "value": "k"}}})
    both("envs bulk write token", "PATCH", "/applications/<app>/envs/bulk", {"data": [{"key": "BULK_WRITE", "value": "w"}]}, tok=WRITE)
    same_rows("rows after bulk", ENV_ROWS, *ENV_DROP)

    # Columns the API does not set.
    for side in SIDES:
        sql(fill(side, "UPDATE environment_variables SET is_required=true WHERE resourceable_id=<app_id> AND resourceable_type='App\\Models\\Application' AND key IN ('NULL_VALUE','PLAIN')"))
        sql(fill(side, "UPDATE environment_variables SET uses_legacy_escaping=true WHERE resourceable_id=<app_id> AND resourceable_type='App\\Models\\Application' AND key IN ('JSON_V','FLAGS.A','PLAIN','BULK_NEW')"))
    both("envs list required and legacy", "GET", "/applications/<app>/envs")
    cross_read("envs list required and legacy", "/applications/<app>/envs")
    both("envs delete not found", "DELETE", "/applications/<app>/envs/nope")
    both("envs delete other token", "DELETE", "/applications/<app>/envs/nope", tok=READ)
    for side in SIDES:
        ctx[side]["env"] = sql(fill(side, "SELECT uuid FROM environment_variables WHERE resourceable_id=<app_id> AND resourceable_type='App\\Models\\Application' AND key='BULK_WRITE' AND is_preview=false"))
    both("envs delete", "DELETE", "/applications/<app>/envs/<env>")
    both("envs delete again", "DELETE", "/applications/<app>/envs/<env>")
    same_rows("rows after delete keep the preview copy", ENV_ROWS, *ENV_DROP)


def storage_cases(args):
    vm = lambda cmd: subprocess.run(["orb", "-m", args.vm, "-u", "root", "bash", "-c", cmd], capture_output=True, text=True).stdout
    p_rows = "SELECT * FROM local_persistent_volumes WHERE resource_type='App\\Models\\Application' AND resource_id=<app_id> ORDER BY id"
    f_rows = "SELECT * FROM local_file_volumes WHERE resource_type='App\\Models\\Application' AND resource_id=<app_id> ORDER BY id"
    missing = "does-not-exist"
    both("storages list unknown", "GET", f"/applications/{missing}/storages")
    both("storages create unknown", "POST", f"/applications/{missing}/storages", {"type": "file"})
    both("storages patch unknown", "PATCH", f"/applications/{missing}/storages", {"type": "file"})
    both("storages delete unknown", "DELETE", f"/applications/{missing}/storages/x")
    both("storages other team", "GET", "/applications/<app>/storages", tok=OTHER)
    both("storages list empty", "GET", "/applications/<app>/storages")
    both("storages create read", "POST", "/applications/<app>/storages", {"type": "file"}, tok=READ)
    both("storages create member", "POST", "/applications/<app>/storages", {"type": "file", "mount_path": "/a"}, tok=MEMBER_WRITE)
    both("storages patch member", "PATCH", "/applications/<app>/storages", {"type": "file"}, tok=MEMBER_WRITE)
    both("storages delete member", "DELETE", "/applications/<app>/storages/x", tok=MEMBER_WRITE)
    S = "/applications/<app>/storages"
    for label, body in [
        ("empty type", {"mount_path": "/a"}), ("bad type", {"type": "x", "mount_path": "/a"}), ("no mount", {"type": "file"}),
        ("extra", {"type": "file", "mount_path": "/a", "foo": 1}), ("bad name", {"type": "persistent", "name": "-x", "mount_path": "/a"}),
        ("int content", {"type": "file", "mount_path": "/a", "content": 1}), ("bad bools", {"type": "file", "mount_path": "/a", "is_directory": "x", "is_host_file": 3}),
        ("null fs", {"type": "file", "mount_path": "/a", "fs_path": None}),
        ("persistent no name", {"type": "persistent", "mount_path": "/a"}), ("persistent zero name", {"type": "persistent", "name": "0", "mount_path": "/a"}),
        ("persistent content", {"type": "persistent", "name": "n", "mount_path": "/a", "content": "x", "fs_path": "/b"}),
        ("file name", {"type": "file", "name": "n", "mount_path": "/a"}), ("both kinds", {"type": "file", "mount_path": "/a", "is_directory": True, "is_host_file": True}),
        ("dir no fs", {"type": "file", "mount_path": "/a", "is_directory": True}), ("host no fs", {"type": "file", "mount_path": "/a", "is_host_file": True}),
        ("host content", {"type": "file", "mount_path": "/a", "is_host_file": True, "fs_path": "/x", "content": "c"}),
        ("host relative", {"type": "file", "mount_path": "/a", "is_host_file": True, "fs_path": "x"}),
        ("host dir", {"type": "file", "mount_path": "/a", "is_host_file": True, "fs_path": "/x/"}),
        ("host dots", {"type": "file", "mount_path": "/a", "is_host_file": True, "fs_path": "/x/../y"}),
        ("host shell", {"type": "file", "mount_path": "/a", "is_host_file": True, "fs_path": "/x;y"}),
        ("host bad mount", {"type": "file", "mount_path": "/a/../b", "is_host_file": True, "fs_path": "/x"}),
        ("file traversal", {"type": "file", "mount_path": "/a/../b"}), ("file shell", {"type": "file", "mount_path": "/a$(id)"}),
        ("file backslash", {"type": "file", "mount_path": "/a\\b"}), ("file pipe", {"type": "file", "mount_path": "/a|b"}),
        ("fs tilde", {"type": "file", "mount_path": "/a", "fs_path": "~/x"}), ("fs relative escape", {"type": "file", "mount_path": "/a", "fs_path": "../x"}),
        ("fs root", {"type": "file", "mount_path": "/a", "fs_path": "/"}), ("fs dots", {"type": "file", "mount_path": "/a", "fs_path": "/x/./y"}),
        ("dir tilde", {"type": "file", "mount_path": "/a", "is_directory": True, "fs_path": "~/x"}),
        ("dir bad mount", {"type": "file", "mount_path": "/a/..", "is_directory": True, "fs_path": "sub"}),
    ]:
        both(f"storages create invalid: {label}", "POST", S, body)
    both("storages create persistent", "POST", S, {"type": "persistent", "name": "data", "mount_path": "//var/lib/data"})
    both("storages create duplicate name", "POST", S, {"type": "persistent", "name": "data", "mount_path": "/x"}, status_only=True)
    both("storages persistent as member read", "GET", S, tok=MEMBER_READ)
    both("storages file content needs deploy", "POST", S, {"type": "file", "mount_path": "/etc/app.conf", "content": "x"}, tok=WRITE)
    both("storages file empty with write", "POST", S, {"type": "file", "mount_path": "/etc/empty.conf"}, tok=WRITE)
    both("storages file with content", "POST", S, {"type": "file", "mount_path": "etc//app.conf", "content": "line1\nline2\n"})
    both("storages file with content, write response hides", "POST", S, {"type": "file", "mount_path": "/etc/none.conf", "content": None}, tok=WRITE)
    both("storages duplicate mount", "POST", S, {"type": "file", "mount_path": "/etc/app.conf"}, status_only=True)
    both("storages dir inside", "POST", S, {"type": "file", "mount_path": "/srv/dir", "is_directory": True, "fs_path": "sub/dir"}, tok=WRITE)
    both("storages dir outside needs deploy", "POST", S, {"type": "file", "mount_path": "/srv/out", "is_directory": True, "fs_path": "/tmp/subdiff-out-<app>"}, tok=WRITE)
    both("storages dir outside", "POST", S, {"type": "file", "mount_path": "/srv/out", "is_directory": True, "fs_path": "/tmp/subdiff-out-<app>"})
    both("storages file fs_path outside needs deploy", "POST", S, {"type": "file", "mount_path": "/srv/f1", "fs_path": "/tmp/subdiff-f1-<app>"}, tok=WRITE)
    both("storages file fs_path outside", "POST", S, {"type": "file", "mount_path": "/srv/f1", "fs_path": "/tmp/subdiff-f1-<app>", "content": "outside"})
    both("storages file fs_path inside", "POST", S, {"type": "file", "mount_path": "/srv/f2", "fs_path": "./rel/f2", "content": "inside"})
    both("storages host file", "POST", S, {"type": "file", "mount_path": "/srv/host", "is_host_file": True, "fs_path": "//etc//hostname"}, tok=WRITE)
    same_rows("persistent rows", p_rows, *STORAGE_DROP)
    same_rows("file rows", f_rows, *STORAGE_DROP)
    for tok in (ROOT_TOK, WRITE, READ, MEMBER_READ):
        both(f"storages list as {tok}", "GET", S, tok=tok)
    cross_read("storages list", S)

    # The server writes are queued on both sides.
    time.sleep(8)
    listing = {}
    for side in SIDES:
        u = ctx[side]["app"]
        listing[side] = vm(f"cd /data/coolify/applications/{u} 2>/dev/null && find . | sort && for f in etc/app.conf rel/f2 etc/empty.conf; do echo \"== $f\"; stat -c '%a' $f; cat $f; done; ls -d /tmp/subdiff-out-{u} /tmp/subdiff-f1-{u} 2>&1 | sed 's/{u}/APP/'")
    record("files on the VM", listing["ref"] == listing["rs"] and "app.conf" in listing["ref"], f"ref {listing['ref']}\n  rs  {listing['rs']}")

    for side in SIDES:
        ctx[side]["pid"] = sql(fill(side, "SELECT id FROM local_persistent_volumes WHERE resource_id=<app_id> AND resource_type='App\\Models\\Application' LIMIT 1"))
        ctx[side]["puuid"] = sql(fill(side, "SELECT uuid FROM local_persistent_volumes WHERE resource_id=<app_id> AND resource_type='App\\Models\\Application' LIMIT 1"))
        for name, mount in [("fid", "/etc/app.conf"), ("dirfile", "/srv/dir"), ("empty", "/etc/empty.conf"), ("hostf", "/srv/host"), ("outside", "/srv/out"), ("inside", "/srv/f2")]:
            ctx[side][name] = sql(fill(side, f"SELECT uuid FROM local_file_volumes WHERE resource_id=<app_id> AND resource_type='App\\Models\\Application' AND mount_path='{mount}'"))
        ctx[side]["fnum"] = sql(fill(side, "SELECT id FROM local_file_volumes WHERE resource_id=<app_id> AND resource_type='App\\Models\\Application' AND mount_path='/etc/app.conf'"))

    for label, body in [
        ("no type", {"uuid": "x"}), ("no lookup", {"type": "file"}), ("not found", {"type": "file", "uuid": "nope"}),
        ("not found by id", {"type": "persistent", "id": 999999999}), ("bad id", {"type": "file", "id": "x"}),
        ("extra", {"type": "file", "uuid": "<fid>", "foo": 1}), ("host_path not allowed", {"type": "file", "uuid": "<fid>", "host_path": "/x"}),
        ("wrong type", {"type": "persistent", "uuid": "<fid>"}), ("bad name", {"type": "persistent", "uuid": "<puuid>", "name": "-x"}),
        ("persistent content", {"type": "persistent", "uuid": "<puuid>", "content": "x"}), ("file name", {"type": "file", "uuid": "<fid>", "name": "n"}),
        ("bad suffix", {"type": "file", "uuid": "<fid>", "is_preview_suffix_enabled": "x"}),
    ]:
        both(f"storages patch invalid: {label}", "PATCH", S, body)
    both("storages patch persistent", "PATCH", S, {"type": "persistent", "uuid": "<puuid>", "name": "renamed", "mount_path": "new/path", "is_preview_suffix_enabled": False})
    both("storages patch persistent by id", "PATCH", S, {"type": "persistent", "id": "<pid>", "is_preview_suffix_enabled": 1})
    both("storages patch nothing changes", "PATCH", S, {"type": "persistent", "uuid": "<puuid>", "is_preview_suffix_enabled": True})
    both("storages patch content needs deploy", "PATCH", S, {"type": "file", "uuid": "<fid>", "content": "changed"}, tok=WRITE)
    both("storages patch same content with write", "PATCH", S, {"type": "file", "uuid": "<fid>", "content": "line1\nline2"}, tok=WRITE)
    both("storages patch content", "PATCH", S, {"type": "file", "uuid": "<fid>", "content": "changed\n"})
    both("storages patch mount", "PATCH", S, {"type": "file", "uuid": "<fid>", "mount_path": "/etc/moved.conf"}, tok=WRITE)
    both("storages patch dir ignores content check", "PATCH", S, {"type": "file", "uuid": "<dirfile>", "content": "x"}, tok=WRITE)
    both("storages patch host file content", "PATCH", S, {"type": "file", "uuid": "<hostf>", "content": None}, tok=WRITE)
    both("storages patch content null", "PATCH", S, {"type": "file", "uuid": "<empty>", "content": None})
    both("storages patch by id", "PATCH", S, {"type": "file", "id": "<fnum>", "is_preview_suffix_enabled": False})
    same_rows("persistent rows after patch", p_rows, *STORAGE_DROP)
    same_rows("file rows after patch", f_rows, *STORAGE_DROP)
    cross_read("storages list after patch", S)
    time.sleep(6)
    c = {}
    for side in SIDES:
        c[side] = vm(f"cat /data/coolify/applications/{ctx[side]['app']}/etc/app.conf")
    record("patched content on the VM", c["ref"] == c["rs"] and "changed" in c["ref"], f"ref {c['ref']!r} rs {c['rs']!r}")

    # Backups block the delete (only the check: the routes themselves are out of scope).
    for side in SIDES:
        sql(fill(side, "INSERT INTO scheduled_volume_backups (uuid, backupable_type, backupable_id, team_id, frequency) VALUES ('subdiff-<app>', 'App\\Models\\LocalPersistentVolume', <pid>, (SELECT team_id FROM projects WHERE uuid='<project>'), '0 0 * * *')"))
    both("storages delete with backups", "DELETE", S + "/<puuid>")
    for side in SIDES:
        sql(fill(side, "DELETE FROM scheduled_volume_backups WHERE uuid='subdiff-<app>'"))

    # Additional servers refuse persistent storage.
    for side in SIDES:
        sql(fill(side, "INSERT INTO additional_destinations (application_id, server_id, standalone_docker_id) SELECT <app_id>, s.server_id, s.id FROM standalone_dockers s WHERE s.id=(SELECT destination_id FROM applications WHERE id=<app_id>)"))
    both("storages persistent with additional server", "POST", S, {"type": "persistent", "name": "second", "mount_path": "/second"})
    for side in SIDES:
        sql(fill(side, "DELETE FROM additional_destinations WHERE application_id=<app_id>"))

    both("storages delete not found", "DELETE", S + "/nope")
    both("storages delete persistent", "DELETE", S + "/<puuid>")
    both("storages delete file", "DELETE", S + "/<fid>")
    both("storages delete directory", "DELETE", S + "/<dirfile>")
    both("storages delete outside", "DELETE", S + "/<outside>")
    both("storages delete host file", "DELETE", S + "/<hostf>")
    both("storages delete relative", "DELETE", S + "/<inside>")
    same_rows("rows after storage deletes", f_rows, *STORAGE_DROP)
    gone = {}
    for side in SIDES:
        u = ctx[side]["app"]
        gone[side] = vm(f"cd /data/coolify/applications/{u} 2>/dev/null && find . | sort; ls -d /tmp/subdiff-out-{u} 2>&1 | sed 's/{u}/APP/'")
    record("VM after deletes", gone["ref"] == gone["rs"], f"ref {gone['ref']}\n  rs  {gone['rs']}")


def tag_cases(base):
    missing = "does-not-exist"
    T = "/applications/<app>/tags"
    both("tags list unknown", "GET", f"/applications/{missing}/tags")
    both("tags create unknown", "POST", f"/applications/{missing}/tags", {"tag_name": "ab"})
    both("tags delete unknown", "DELETE", f"/applications/{missing}/tags/x")
    both("tags other team", "GET", T, tok=OTHER)
    both("tags list empty", "GET", T)
    both("tags create read", "POST", T, {"tag_name": "ab"}, tok=READ)
    both("tags create member", "POST", T, {"tag_name": "ab"}, tok=MEMBER_WRITE)
    both("tags delete member", "DELETE", T + "/x", tok=MEMBER_WRITE)
    for label, body in [
        ("empty body", {}), ("both", {"tag_name": "ab", "tag_names": ["cd"]}), ("null name", {"tag_name": None}), ("null names", {"tag_names": None}),
        ("int name", {"tag_name": 5}), ("string names", {"tag_names": "x"}), ("empty names", {"tag_names": []}), ("object names empty", {"tag_names": {}}),
        ("bad items", {"tag_names": ["ok", 5, None]}), ("extra", {"tag_name": "ab", "foo": 1}), ("only extra", {"foo": 1}),
        ("short", {"tag_name": "a"}), ("short after strip", {"tag_name": "<b>a</b>"}), ("short item", {"tag_names": ["subdiffok", "x"]}),
    ]:
        both(f"tags create invalid: {label}", "POST", T, body)
    both("tags create one", "POST", T, {"tag_name": " <i>SubDiff-One</i> "})
    both("tags create many", "POST", T, {"tag_names": ["subdiff-two", "SUBDIFF-TWO", "subdiff-three"]})
    both("tags create again", "POST", T, {"tag_name": "subdiff-one"})
    both("tags create keyed", "POST", T, {"tag_names": {"a": "subdiff-four"}})
    both("tags list", "GET", T)
    both("tags list member", "GET", T, tok=MEMBER_READ)
    cross_read("tags list", T)
    pivot = "SELECT t.name FROM taggables p JOIN tags t ON t.id=p.tag_id WHERE p.taggable_type='App\\Models\\Application' AND p.taggable_id=<app_id> ORDER BY t.name"
    same_rows("taggables rows", pivot)
    for side in SIDES:
        ctx[side]["tag"] = sql(fill(side, "SELECT t.uuid FROM taggables p JOIN tags t ON t.id=p.tag_id WHERE p.taggable_type='App\\Models\\Application' AND p.taggable_id=<app_id> AND t.name='subdiff-two'"))

    def named(side, query):
        status, raw = call(side, "GET", "/api/v1/applications" + query, None, ROOT_TOK)
        data = parsed(raw)
        return status, sorted(a["name"] for a in data) if isinstance(data, list) else data

    a, b = named("ref", "?tag=subdiff-one"), named("rs", "?tag=subdiff-one")
    record("list by tag", a == b and a[0] == 200 and len(a[1]) >= 2, f"ref {a}\n  rs  {b}")
    a, b = named("ref", "?tag=subdiff-none"), named("rs", "?tag=subdiff-none")
    record("list by unknown tag", a == b and a[1] == [], f"ref {a}\n  rs  {b}")
    a, b = named("ref", "?tag="), named("rs", "?tag=")
    record("list with empty tag", a == b, f"ref {a}\n  rs  {b}")
    both("tag delete unknown", "DELETE", T + "/nope")
    both("tag delete", "DELETE", T + "/<tag>")
    both("tag delete not on resource", "DELETE", T + "/<tag>")
    tag_left = sql("SELECT count(*) FROM tags WHERE name='subdiff-two'")
    record("the tag stays after detach", tag_left == "1", tag_left)
    both("tags list after delete", "GET", T)
    a, b = named("ref", "?tag=subdiff-two"), named("rs", "?tag=subdiff-two")
    record("list by detached tag", a == b and a[1] == [], f"ref {a}\n  rs  {b}")

    # `tags` on create.
    img = dict(base, docker_registry_image_name="nginx:1.27-alpine", ports_exposes="80")
    both("create with short tag", "POST", "/applications/dockerimage", dict(img, tags=["x"]))
    both("create with non-array tags", "POST", "/applications/dockerimage", dict(img, tags="x"))
    both("create with null tags", "POST", "/applications/dockerimage", dict(img, tags=None), status_only=True)
    both("create with nested tags", "POST", "/applications/dockerimage", dict(img, tags=[["a"]]), status_only=True)
    r = both("create with tags", "POST", "/applications/dockerimage", dict(img, name="subdiff-tagged", tags=[" SubDiff-Created ", "subdiff-one"]))
    for side in SIDES:
        ctx[side]["tagged"] = r[side]["uuid"] if isinstance(r[side], dict) and "uuid" in r[side] else ""
    both("tags of created app", "GET", "/applications/<tagged>/tags")


def secret_manager_cases():
    S = "/applications/<app>/secret-manager"
    code = """
$make = function ($provider, $token, $caps) { return \\App\\Models\\IntegrationToken::create(["team_id" => %d, "provider" => $provider, "name" => "subdiff", "token" => $token, "capabilities" => $caps, "metadata" => []])->uuid; };
echo json_encode(["doppler_sa" => $make("doppler", "dp.sa.fake", ["secrets"]), "doppler_st" => $make("doppler", "dp.st.fake", ["secrets"]), "infisical" => $make("infisical", "fake", ["secrets"]), "vault" => $make("vault", "fake", ["secrets"]), "nosecrets" => $make("vault", "fake", []), "cloudflare" => $make("cloudflare", "fake", ["secrets"])]);
""" % int(json.load(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "../reference/secrets/tokens.json")))["acme_team_id"])
    out = tinker(code)
    tokens = json.loads(out[out.index("{"):out.rindex("}") + 1])
    for side in SIDES:
        ctx[side].update({f"tok_{k}": v for k, v in tokens.items()})
    try:
        both("secret manager unknown app", "PATCH", "/applications/does-not-exist/secret-manager", {"integration_token_uuid": "x"})
        both("secret manager read", "PATCH", S, {"integration_token_uuid": "x"}, tok=READ)
        both("secret manager member", "PATCH", S, {"integration_token_uuid": "x"}, tok=MEMBER_WRITE)
        both("secret manager empty", "PATCH", S, {})
        both("secret manager no token", "PATCH", S, {"settings": {}})
        both("secret manager unknown token", "PATCH", S, {"integration_token_uuid": "x"})
        both("secret manager no capability", "PATCH", S, {"integration_token_uuid": "<tok_nosecrets>"})
        both("secret manager other provider", "PATCH", S, {"integration_token_uuid": "<tok_cloudflare>"})
        both("secret manager other team", "PATCH", S, {"integration_token_uuid": "<tok_vault>"}, tok=OTHER)
        both("secret manager extra", "PATCH", S, {"integration_token_uuid": "<tok_doppler_st>", "foo": 1})
        both("secret manager bad settings", "PATCH", S, {"integration_token_uuid": "<tok_doppler_st>", "settings": "x"})
        both("doppler sa missing", "PATCH", S, {"integration_token_uuid": "<tok_doppler_sa>"})
        both("doppler sa not string", "PATCH", S, {"integration_token_uuid": "<tok_doppler_sa>", "settings": {"project": 1, "config": "c"}})
        both("infisical missing", "PATCH", S, {"integration_token_uuid": "<tok_infisical>", "settings": {"secret_path": 3}})
        both("vault missing", "PATCH", S, {"integration_token_uuid": "<tok_vault>", "settings": {"mount": "m"}})
        link = "SELECT resourceable_type, integration_token_id, settings::text FROM secret_manager_links WHERE resourceable_type='App\\Models\\Application' AND resourceable_id=<app_id>"
        both("doppler service", "PATCH", S, {"integration_token_uuid": "<tok_doppler_st>", "settings": {"anything": "x", "blank": "", "zero": 0, "list": []}})
        same_rows("link row doppler service", link)
        both("doppler service no settings", "PATCH", S, {"integration_token_uuid": "<tok_doppler_st>"})
        same_rows("link row no settings", link)
        both("doppler service account", "PATCH", S, {"integration_token_uuid": "<tok_doppler_sa>", "settings": {"config": "c", "project": "p", "other": "dropped"}})
        same_rows("link row doppler account", link)
        both("infisical", "PATCH", S, {"integration_token_uuid": "<tok_infisical>", "settings": {"environment": "e", "project_id": "p", "secret_path": "/a/b"}})
        same_rows("link row infisical", link)
        both("vault", "PATCH", S, {"integration_token_uuid": "<tok_vault>", "settings": {"path": "p", "mount": "m"}})
        same_rows("link row vault", link)
    finally:
        for side in SIDES:
            sql(fill(side, "DELETE FROM secret_manager_links WHERE resourceable_type='App\\Models\\Application' AND resourceable_id=<app_id>"))
        sql("DELETE FROM integration_tokens WHERE name='subdiff'")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", required=True)
    ap.add_argument("--vm", required=True)
    args = ap.parse_args()
    project = json.loads(call("ref", "POST", "/api/v1/projects", {"name": "subresources-diff"}, ROOT_TOK)[1])["uuid"]
    base = {"server_uuid": args.server, "project_uuid": project, "environment_name": "production"}
    created = []
    shared = []
    try:
        for side in SIDES:
            status, raw = call(side, "POST", "/api/v1/applications/dockerimage",
                               dict(base, name="subdiff-app", docker_registry_image_name="nginx:1.27-alpine", ports_exposes="80"), ROOT_TOK)
            if status != 201:
                raise SystemExit(f"{side}: could not create the application: {status} {raw[:300]}")
            uuid = json.loads(raw)["uuid"]
            created.append((side, uuid))
            ctx[side].update(app=uuid, project=project, app_id=sql(f"SELECT id FROM applications WHERE uuid='{uuid}'"))
        for key, once in [("SUBDIFF_SHARED", False), ("SUBDIFF_LOCKED", True)]:
            status, raw = call("ref", "POST", "/api/v1/team/envs", {"key": key, "value": f"shared-{key}", "is_shown_once": once}, ROOT_TOK)
            if status not in (200, 201):
                raise SystemExit(f"shared variable: {status} {raw[:300]}")
            shared.append(key)
        env_cases()
        storage_cases(args)
        tag_cases(base)
        secret_manager_cases()
    finally:
        for side in SIDES:
            tagged = ctx[side].get("tagged")
            if tagged:
                created.append((side, tagged))
        for side, uuid in created:
            sql(f"DELETE FROM local_file_volumes WHERE resource_type='App\\Models\\Application' AND resource_id=(SELECT id FROM applications WHERE uuid='{uuid}')")
            call(side, "DELETE", f"/api/v1/applications/{uuid}?delete_volumes=false&delete_connected_networks=false", None, ROOT_TOK)
        for _ in range(60):
            if sql("SELECT count(*) FROM applications WHERE uuid IN (%s)" % ",".join(f"'{u}'" for _, u in created) if created else "SELECT 0") == "0":
                break
            time.sleep(2)
        for key in shared:
            sql(f"DELETE FROM shared_environment_variables WHERE key='{key}'")
        sql("DELETE FROM tags WHERE name LIKE 'subdiff-%'")
        sql("DELETE FROM integration_tokens WHERE name='subdiff'")
        for _, uuid in created:
            subprocess.run(["orb", "-m", args.vm, "-u", "root", "bash", "-c",
                            f"rm -rf /data/coolify/applications/{uuid} /tmp/subdiff-out-{uuid} /tmp/subdiff-f1-{uuid}"], capture_output=True)
        call("ref", "DELETE", f"/api/v1/projects/{project}", None, ROOT_TOK)
    passed = sum(1 for r in results if r)
    print(f"{passed}/{len(results)} checks passed")
    sys.exit(0 if passed == len(results) else 1)


if __name__ == "__main__":
    main()
