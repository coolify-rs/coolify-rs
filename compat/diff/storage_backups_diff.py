#!/usr/bin/env python3
"""Differential check of PUT, DELETE and POST .../run on /databases/{uuid}/storages/{storage_uuid}/backups.

Each side gets its own PostgreSQL database on the test VM. Compares answers (errors, tokens), rows and the archive
a real run writes, with and without stop_during_backup. Everything created is deleted by uuid. Usage:

    COOLIFY_RS_BASE=http://127.0.0.1:18087 compat/diff/storage_backups_diff.py
"""
import json
import os
import re
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import databases_diff as d  # noqa: E402

SERVER = "hml20fsj5eles0xfiqidq8j5"
PROJECT = "j8xm2emlllhfnyebjzgyzp7z"
ENV = "uj3st9yvtvzxh7rul3c3opcz"
ROOT = "acme_owner_root"
failed = []


def vm(cmd):
    return subprocess.run(["orb", "-m", "coolify-rs-e2e", "-u", "root", "bash", "-c", cmd], capture_output=True, text=True).stdout.strip()


def check(name, a, b):
    if a == b:
        print("ok  ", name)
    else:
        failed.append(name)
        print("DIFF", name, "\n  ref", json.dumps(a)[:900], "\n  rs ", json.dumps(b)[:900])


def norm(body, subs):
    text = body.decode() if isinstance(body, bytes) else body
    for k, v in subs.items():
        text = text.replace(k, v)
    text = re.sub(r'"uuid":"[a-z0-9]{24}"', '"uuid":"U"', text)
    try:
        return json.loads(text)
    except ValueError:
        return text


def wait_status(side, uuid, prefix):
    for _ in range(60):
        c, b = d.call(side, "GET", f"/api/v1/databases/{uuid}", tok=ROOT)
        if json.loads(b).get("status", "").startswith(prefix):
            return True
        time.sleep(3)
    return False


def execution_of(backup):
    return json.loads(d.sql(f"select row_to_json(r) from (select e.status, e.filename, e.size, e.message, e.local_storage_deleted, e.s3_uploaded, e.stop_recovery_pending, e.s3_cleanup_pending, e.finished_at is not null as finished, e.stop_container_ids from scheduled_volume_backup_executions e join scheduled_volume_backups b on b.id = e.scheduled_volume_backup_id where b.uuid = '{backup}' order by e.id desc limit 1) r") or "null")


def wait_execution(backup, count):
    for _ in range(50):
        n = int(d.sql(f"select count(*) from scheduled_volume_backup_executions e join scheduled_volume_backups b on b.id = e.scheduled_volume_backup_id where b.uuid = '{backup}'"))
        row = execution_of(backup)
        if n >= count and row and row["status"] != "running" and row["finished"]:
            return row
        time.sleep(2)
    return execution_of(backup)


def schedule_row(backup):
    return json.loads(d.sql(f"select row_to_json(r) from (select backupable_type, team_id, frequency, enabled, save_s3, disable_local_backup, stop_during_backup, s3_storage_id, retention_amount_locally, retention_days_locally, retention_max_storage_locally, retention_amount_s3, retention_days_s3, retention_max_storage_s3, timeout, missing_backup_notification_days from scheduled_volume_backups where uuid = '{backup}') r") or "null")


def side_run(side):
    out = {}
    c, b = d.call(side, "POST", "/api/v1/databases/postgresql", {"server_uuid": SERVER, "project_uuid": PROJECT, "environment_uuid": ENV, "name": f"pages-vol-{side}", "instant_deploy": False}, tok=ROOT)
    uuid = json.loads(b)["uuid"]
    try:
        c, b = d.call(side, "GET", f"/api/v1/databases/{uuid}/storages", tok=ROOT)
        storage = json.loads(b)["persistent_storages"][0]["uuid"]
        subs = {uuid: "DB", storage: "ST", f"pages-vol-{side}": "NAME"}
        path = f"/api/v1/databases/{uuid}/storages/{storage}/backups"

        def req(name, method, p, body=None, tok=ROOT):
            c, b = d.call(side, method, p, body, tok=tok)
            out[name] = [c, norm(b, subs)]
            return c, b

        req("no_db", "PUT", f"/api/v1/databases/nope/storages/{storage}/backups", {"frequency": "daily"})
        req("no_storage", "PUT", f"/api/v1/databases/{uuid}/storages/nope/backups", {"frequency": "daily"})
        req("run_none", "POST", path + "/run")
        req("del_none", "DELETE", path)
        req("del_no_db", "DELETE", f"/api/v1/databases/nope/storages/{storage}/backups")
        req("run_no_db", "POST", f"/api/v1/databases/nope/storages/{storage}/backups/run")
        req("read_token", "PUT", path, {"frequency": "daily"}, tok="acme_owner_read")
        req("run_write_token", "POST", path + "/run", tok="acme_owner_write")
        req("member_token", "PUT", path, {"frequency": "daily"}, tok="acme_member_write_legacy")
        req("stop_no_deploy", "PUT", path, {"frequency": "daily", "stop_during_backup": True}, tok="acme_owner_write")
        req("required", "PUT", path, {})
        req("bad_cron", "PUT", path, {"frequency": "nope"})
        req("extra", "PUT", path, {"frequency": "daily", "other": 1})
        req("types", "PUT", path, {"frequency": 5, "enabled": "x", "save_s3": "x", "retention_amount_locally": "a", "retention_days_locally": -1, "retention_max_storage_locally": "a", "retention_amount_s3": 20000, "timeout": 5, "missing_backup_notification_days": 400, "s3_storage_uuids": "x", "s3_storage_uuid": 5})
        req("local_off", "PUT", path, {"frequency": "daily", "disable_local_backup": True})
        req("s3_missing", "PUT", path, {"frequency": "daily", "save_s3": True})
        req("s3_list", "PUT", path, {"frequency": "daily", "save_s3": True, "s3_storage_uuids": ["a", "a"]})
        req("s3_unknown", "PUT", path, {"frequency": "daily", "save_s3": True, "s3_storage_uuid": "nope"})
        req("s3_primary", "PUT", path, {"frequency": "daily", "save_s3": True, "s3_storage_uuid": "x", "s3_storage_uuids": ["y"]})

        c, b = req("create", "PUT", path, {"frequency": "daily", "retention_amount_locally": 3, "retention_max_storage_locally": 1.5})
        backup = json.loads(b)["uuid"]
        out["row_create"] = schedule_row(backup)
        c, b = req("update", "PUT", path, {"frequency": "0 2 * * *", "enabled": False, "timeout": 600, "missing_backup_notification_days": 3})
        out["row_update"] = schedule_row(backup)
        same_uuid = json.loads(b)["uuid"] == backup
        out["same_uuid"] = same_uuid

        # A real run with the database stopped: nothing to stop for.
        d.call(side, "POST", f"/api/v1/databases/{uuid}/start", tok=ROOT)
        out["started"] = wait_status(side, uuid, "running")
        req("run", "POST", path + "/run")
        ex = wait_execution(backup, 1)
        name = ex.get("filename") or ""
        out["execution"] = {**ex, "filename": re.sub(r"-\d{9,}\.tar\.gz$", "-T.tar.gz", name.replace(uuid, "DB").replace(storage, "ST")), "size": int(ex["size"] or 0) > 0}
        out["file_exists"] = vm(f"test -s '{name}' && echo yes || echo no")
        out["tar_has_pg_version"] = vm(f"tar -tzf '{name}' | grep -c 'PG_VERSION'")

        # stop_during_backup: the database goes down for the archive and comes back.
        req("stop_set", "PUT", path, {"frequency": "daily", "stop_during_backup": True})
        req("run_stop", "POST", path + "/run")
        ex2 = wait_execution(backup, 2)
        out["execution_stop"] = {"status": ex2["status"], "pending": ex2["stop_recovery_pending"], "ids": ex2["stop_container_ids"], "message": ex2["message"]}
        out["running_after_stop"] = wait_status(side, uuid, "running")
        out["container_after_stop"] = vm(f"docker inspect --format '{{{{.State.Running}}}}' {uuid}")
        out["state_file_gone"] = vm("ls /tmp/coolify-volume-backup-* 2>/dev/null | wc -l")

        # Retention: keep one.
        req("keep_one", "PUT", path, {"frequency": "daily", "retention_amount_locally": 1})
        req("run_keep", "POST", path + "/run")
        wait_execution(backup, 3)
        time.sleep(4)
        out["executions_after_retention"] = int(d.sql(f"select count(*) from scheduled_volume_backup_executions e join scheduled_volume_backups b on b.id = e.scheduled_volume_backup_id where b.uuid = '{backup}'"))
        files = d.sql(f"select e.filename from scheduled_volume_backup_executions e join scheduled_volume_backups b on b.id = e.scheduled_volume_backup_id where b.uuid = '{backup}'").split("\n")

        req("delete", "DELETE", path)
        out["row_deleted"] = schedule_row(backup)
        out["files_gone"] = [vm(f"test -e '{f}' && echo present || echo gone") for f in files if f]
        out["first_file_gone"] = vm(f"test -e '{name}' && echo present || echo gone")
    finally:
        d.call(side, "DELETE", f"/api/v1/databases/{uuid}?delete_volumes=true&delete_configurations=true&docker_cleanup=true", tok=ROOT)
    return out


ref = side_run("ref")
rs = side_run("rs")
for key in ref:
    check(key, ref[key], rs.get(key))
print("failed:", failed)
sys.exit(1 if failed else 0)
