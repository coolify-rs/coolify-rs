#!/usr/bin/env python3
"""S3 paths of database and volume backups against a local MinIO, upstream vs coolify-rs.

    scripts/dev-server-container.sh 18090
    COOLIFY_RS_BASE=http://127.0.0.1:18090 s3_backup_diff.py [--case standalone,volume,service]

MinIO runs on the test VM as `coolify-minio` on the `coolify` network; its image is copied from this Mac's Docker.
A socat sidecar with the same name on the reference's Docker network forwards to it, so the reference and an instance
on that network resolve it. The S3 row has upstream's trusted uuid `minio`, so `instance_settings` stays as it is.
Not run here because they need `instance_settings`: `PreparesBackupDownloads` and S3 file downloads
(`file_download_s3_storage_id`). Everything created is tracked and deleted, also after a failure.
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
from databases_diff import call as raw_call, record, results, sql  # noqa: E402
from rs_session import Rs  # noqa: E402

VM1 = "coolify-rs-e2e"
SERVER = "hml20fsj5eles0xfiqidq8j5"
MINIO = "coolify-minio"
KEY, SECRET, BUCKET = "s3diffkey", "s3diffsecret123", "s3diff"
NET = "coolify-rs-reference_default"
HELPER = "coollabsio/coolify-helper:1.0.19"
SERVICE_DBS = {
    "mysql": ("mysql:8.4", "MYSQL_ROOT_PASSWORD=s3diffpw\n      - MYSQL_DATABASE=appdb", "/var/lib/mysql"),
    "mariadb": ("mariadb:11", "MARIADB_ROOT_PASSWORD=s3diffpw\n      - MARIADB_DATABASE=appdb", "/var/lib/mysql"),
    "mongo": ("mongo:7", "MONGO_INITDB_ROOT_USERNAME=root", "/data/db"),
}


def call(side, method, path, body=None):
    c, b = raw_call(side, method, "/api/v1" + path, body, "acme_owner_root")
    try:
        return c, json.loads(b)
    except ValueError:
        return c, b.decode(errors="replace")


def vm(cmd):
    return subprocess.run(["orb", "-m", VM1, "-u", "root", "bash", "-c", cmd], capture_output=True, text=True).stdout


def objects():
    """Every object in the bucket, by key."""
    out = vm(f"docker run --rm --network coolify {HELPER} sh -c 'mc alias set x http://{MINIO}:9000 {KEY} {SECRET} >/dev/null; mc ls -r x/{BUCKET}'")
    return [line.split()[-1] for line in out.splitlines() if line.strip()]


def tinker(code):
    return subprocess.run(["docker", "exec", "coolify-rs-reference-coolify-1", "php", "/var/www/html/artisan", "tinker", "--execute", code],
                          capture_output=True, text=True).stdout


def wait_row(query, tries=120):
    row = None
    for _ in range(tries):
        out = sql(query)
        row = json.loads(out) if out else None
        if row and row.get("status") not in ("running", "in_progress", "queued", None):
            return row
        time.sleep(3)
    return row


class Env:
    def __init__(self):
        self.project = None
        self.databases, self.services = [], []
        self.s3_created = False
        self.image_loaded = False

    def prepare(self):
        # Docker Hub refuses the pull here; the image is in this Mac's Docker.
        if not vm("docker images -q minio/minio:latest").strip():
            saved = subprocess.Popen(["docker", "save", "minio/minio:latest"], stdout=subprocess.PIPE)
            subprocess.run(["orb", "-m", VM1, "-u", "root", "docker", "load"], stdin=saved.stdout, capture_output=True)
            saved.wait()
            self.image_loaded = True
        vm(f"docker inspect {MINIO} >/dev/null 2>&1 || docker run -d --name {MINIO} --restart=no --network coolify -p 9000:9000 -e MINIO_ROOT_USER={KEY} -e MINIO_ROOT_PASSWORD={SECRET} minio/minio server /data")
        ip = vm("hostname -I | cut -d' ' -f1").strip()
        subprocess.run(["docker", "rm", "-f", MINIO], capture_output=True)
        subprocess.run(["docker", "run", "-d", "--name", MINIO, "--restart=no", "--network", NET, "alpine/socat",
                        "tcp-listen:9000,fork,reuseaddr", f"tcp-connect:{ip}:9000"], capture_output=True)
        time.sleep(8)
        record("minio answers", vm(f"curl -s -o /dev/null -w '%{{http_code}}' http://127.0.0.1:9000/minio/health/live") == "200")
        vm(f"docker run --rm --network coolify {HELPER} sh -c 'mc alias set x http://{MINIO}:9000 {KEY} {SECRET} >/dev/null; mc mb -p x/{BUCKET}'")
        record("no s3 row with the trusted uuid before the run", sql("SELECT count(*) FROM s3_storages WHERE uuid='minio'") == "0")
        tinker(f'$s=new App\\Models\\S3Storage;$s->forceFill(["uuid"=>"minio","name"=>"s3-diff","region"=>"us-east-1","key"=>"{KEY}","secret"=>"{SECRET}","bucket"=>"{BUCKET}","endpoint"=>"http://{MINIO}:9000","team_id"=>4,"is_usable"=>true]);$s->save();')
        self.s3_created = sql("SELECT count(*) FROM s3_storages WHERE uuid='minio'") == "1"
        record("s3 row created", self.s3_created)
        _, p = call("ref", "POST", "/projects", {"name": "s3-backup-diff"})
        self.project = p["uuid"]

    def cleanup(self):
        for u in self.services:
            call("ref", "DELETE", f"/services/{u}?delete_volumes=true")
        for u in self.databases:
            call("ref", "DELETE", f"/databases/{u}?delete_volumes=true")
        time.sleep(30)
        if self.project:
            for _ in range(20):
                if call("ref", "DELETE", f"/projects/{self.project}")[0] in (200, 404):
                    break
                time.sleep(5)
        if self.s3_created:
            sql("DELETE FROM s3_storages WHERE uuid='minio' AND name='s3-diff'")
        subprocess.run(["docker", "rm", "-f", MINIO], capture_output=True)
        vm(f"docker rm -f -v {MINIO} >/dev/null 2>&1")
        # Only the copy this run loaded.
        if self.image_loaded:
            vm("docker rmi minio/minio:latest >/dev/null 2>&1")


def start_postgres(e, side):
    c, r = call("ref", "POST", "/databases/postgresql", {"name": f"s3-{side}", "project_uuid": e.project, "environment_name": "production",
                                                         "server_uuid": SERVER, "instant_deploy": False})
    record(f"[standalone] {side} created", c in (200, 201), r)
    u = r["uuid"]
    e.databases.append(u)
    call(side, "POST", f"/databases/{u}/start")
    for _ in range(60):
        if "accepting" in vm(f"docker exec {u} pg_isready 2>&1"):
            # The backup is skipped while the start activity is open, on both sides.
            time.sleep(20)
            return u
        time.sleep(3)
    return u


def execution(backup_uuid):
    return wait_row("SELECT row_to_json(r) FROM (SELECT e.* FROM scheduled_database_backup_executions e JOIN scheduled_database_backups b "
                    f"ON b.id=e.scheduled_database_backup_id WHERE b.uuid='{backup_uuid}' ORDER BY e.id DESC LIMIT 1) r")


def shape(ex, subs):
    if not ex:
        return None
    name = ex.get("filename") or ""
    for old, new in subs:
        name = name.replace(old, new)
    return {"status": ex["status"], "s3_uploaded": ex.get("s3_uploaded"), "local_storage_deleted": ex.get("local_storage_deleted"),
            "s3_storage_deleted": ex.get("s3_storage_deleted"), "message": ex.get("message"), "finished": bool(ex.get("finished_at")), "filename": re.sub(r"\d{9,}", "T", name)}


def case_standalone(e):
    seen = {}
    for side in ("ref", "rs"):
        u = start_postgres(e, side)
        vm(f"docker exec {u} sh -c 'psql -U postgres -d postgres -c \"CREATE TABLE s3marker(id int)\"'")
        c, r = call(side, "POST", f"/databases/{u}/backups", {"frequency": "daily", "save_s3": True, "s3_storage_uuid": "minio", "backup_now": True})
        record(f"[standalone] {side} backup accepted", c in (200, 201), r)
        if c not in (200, 201):
            continue
        backup = r["uuid"]
        ex = execution(backup)
        seen[side] = shape(ex, [(u, "DB"), (f"s3-{side}", "NAME"), (ex["uuid"] if ex else "-", "EXEC")])
        record(f"[standalone] {side} uploaded", bool(ex) and ex["status"] == "success" and ex["s3_uploaded"] is True, ex)
        keys = [k for k in objects() if u in k]
        record(f"[standalone] {side} object in the bucket", len(keys) == 1, keys)
        # S3 import: drop the table, restore from the bucket.
        vm(f"docker exec {u} sh -c 'psql -U postgres -d postgres -c \"DROP TABLE s3marker\"'")
        path = (ex or {}).get("filename") or ""
        c, r = call(side, "POST", f"/databases/{u}/imports", {"source": "s3", "s3_storage_uuid": "minio", "path": path})
        seen[f"{side}.import"] = c
        record(f"[standalone] {side} S3 import accepted", c in (200, 201, 202), r)
        found = ""
        for _ in range(40):
            found = vm(f"""docker exec {u} psql -U postgres -d postgres -Atc "select to_regclass('s3marker')" """).strip()
            if found == "s3marker":
                break
            time.sleep(3)
        record(f"[standalone] {side} S3 import restored the table", found == "s3marker", found)
        # delete_s3
        c, r = call(side, "DELETE", f"/databases/{u}/backups/{backup}/executions/{ex['uuid']}?delete_s3=true")
        seen[f"{side}.delete"] = c
        record(f"[standalone] {side} execution deleted", c == 200, r)
        time.sleep(3)
        keys = [k for k in objects() if u in k]
        record(f"[standalone] {side} delete_s3 removed the object", not keys, keys)
    record("[standalone] executions alike", seen.get("ref") == seen.get("rs"), (seen.get("ref"), seen.get("rs")))
    record("[standalone] answers alike", (seen.get("ref.import"), seen.get("ref.delete")) == (seen.get("rs.import"), seen.get("rs.delete")), seen)


def case_volume(e):
    seen = {}
    for side in ("ref", "rs"):
        u = next((x for x in e.databases if sql(f"SELECT name FROM standalone_postgresqls WHERE uuid='{x}'") == f"s3-{side}"), None) or start_postgres(e, side)
        _, storages = call("ref", "GET", f"/databases/{u}/storages")
        rows = storages.get("persistent_storages", storages) if isinstance(storages, dict) else storages
        st = rows[0]["uuid"]
        path = f"/databases/{u}/storages/{st}/backups"
        c, r = call(side, "PUT", path, {"frequency": "daily", "save_s3": True, "s3_storage_uuid": "minio"})
        record(f"[volume] {side} schedule accepted", c in (200, 201), r)
        c, r = call(side, "POST", path + "/run")
        record(f"[volume] {side} run accepted", c in (200, 201, 202), r)
        ex = wait_row("SELECT row_to_json(r) FROM (SELECT e.* FROM scheduled_volume_backup_executions e JOIN scheduled_volume_backups b ON b.id=e.scheduled_volume_backup_id "
                      f"JOIN local_persistent_volumes v ON v.id=b.backupable_id AND b.backupable_type LIKE '%LocalPersistentVolume' WHERE v.uuid='{st}' ORDER BY e.id DESC LIMIT 1) r")
        record(f"[volume] {side} uploaded", bool(ex) and ex["status"] == "success" and ex.get("s3_uploaded") is True, ex)
        seen[side] = shape(ex, [(u, "DB"), (st, "ST"), (f"s3-{side}", "NAME"), (ex["uuid"] if ex else "-", "EXEC")])
        keys = [k for k in objects() if u in k and "volume" in k.lower()] or [k for k in objects() if st in k]
        record(f"[volume] {side} object in the bucket", bool(keys), objects()[:6])
    record("[volume] executions alike", seen.get("ref") == seen.get("rs"), (seen.get("ref"), seen.get("rs")))


def case_service(e):
    """Dumps of MySQL, MariaDB and Mongo inside a service: tinker on the reference, the page's route on coolify-rs."""
    rs = Rs()
    for kind, (image, env, mount) in SERVICE_DBS.items():
        seen = {}
        for side in ("ref", "rs"):
            environment = [env, "MONGO_INITDB_ROOT_PASSWORD=s3diffpw"] if kind == "mongo" else [env]
            compose = "services:\n  db:\n    image: " + image + "\n    environment:\n" + "".join(f"      - {x}\n" for x in environment) + f"    volumes:\n      - dbdata:{mount}\n"
            c, r = call("ref", "POST", "/services", {"name": f"s3svc-{kind}-{side}", "project_uuid": e.project, "environment_name": "production", "server_uuid": SERVER,
                                                     "docker_compose_raw": base64.b64encode(compose.encode()).decode(), "instant_deploy": False})
            record(f"[service {kind}] {side} created", c == 201, r)
            if c != 201:
                continue
            u = r["uuid"]
            e.services.append(u)
            call(side, "POST", f"/services/{u}/start")
            _, shown = call("ref", "GET", f"/services/{u}")
            dbuuid = next(x["uuid"] for x in shown["databases"])
            for _ in range(60):
                if "healthy" in sql(f"SELECT status FROM service_databases WHERE uuid='{dbuuid}'") or vm(f"docker ps -q --filter name=db-{u}").strip():
                    break
                time.sleep(3)
            time.sleep(25)
            sql(f"UPDATE service_databases SET status='running:healthy' WHERE uuid='{dbuuid}'")
            if side == "ref":
                out = tinker(f'$d=App\\Models\\ServiceDatabase::where("uuid","{dbuuid}")->first();$b=new App\\Models\\ScheduledDatabaseBackup;$b->forceFill(["uuid"=>"sb{dbuuid}","frequency"=>"daily","database_id"=>$d->id,"database_type"=>$d->getMorphClass(),"team_id"=>4,"enabled"=>false,"save_s3"=>false]);$b->save();App\\Jobs\\DatabaseBackupJob::dispatchSync($b);echo "done";')
                backup = f"sb{dbuuid}"
                record(f"[service {kind}] ref ran", "done" in out, out[-300:])
            else:
                code, b = rs.post(f"/ui/databases/{dbuuid}/backups", {"frequency": "daily"})
                record(f"[service {kind}] rs schedule accepted", code == 200, b[:200])
                backup = sql(f"SELECT uuid FROM scheduled_database_backups WHERE database_id=(SELECT id FROM service_databases WHERE uuid='{dbuuid}') AND database_type LIKE '%ServiceDatabase' ORDER BY id DESC LIMIT 1")
                sql(f"UPDATE scheduled_database_backups SET enabled=false WHERE uuid='{backup}'")
                code, b = rs.post(f"/ui/databases/{dbuuid}/backups/{backup}/action", {"action": "backupNow"})
                record(f"[service {kind}] rs backup accepted", code == 200, b[:300])
            ex = execution(backup)
            seen[side] = shape(ex, [(dbuuid, "DB"), (u, "SVC"), (f"s3svc-{kind}-{side}", "NAME"), (ex["uuid"] if ex else "-", "EXEC")])
            record(f"[service {kind}] {side} dump", bool(ex) and ex["status"] == "success" and int(ex.get("size") or 0) > 0, ex)
            sql(f"DELETE FROM scheduled_database_backup_executions WHERE scheduled_database_backup_id=(SELECT id FROM scheduled_database_backups WHERE uuid='{backup}')")
            sql(f"DELETE FROM scheduled_database_backups WHERE uuid='{backup}'")
        record(f"[service {kind}] executions alike", seen.get("ref") == seen.get("rs"), (seen.get("ref"), seen.get("rs")))


def case_stop(e):
    """stop_during_backup on a service volume, then Retry recovery of a stopped container."""
    rs = Rs()
    seen = {}
    compose = "services:\n  web:\n    image: nginx:alpine\n    volumes:\n      - webdata:/data\n"
    for side in ("ref", "rs"):
        c, r = call("ref", "POST", "/services", {"name": f"s3stop-{side}", "project_uuid": e.project, "environment_name": "production", "server_uuid": SERVER,
                                                 "docker_compose_raw": base64.b64encode(compose.encode()).decode(), "instant_deploy": False})
        record(f"[stop] {side} created", c == 201, r)
        u = r["uuid"]
        e.services.append(u)
        call(side, "POST", f"/services/{u}/start")
        for _ in range(40):
            if vm(f"docker ps -q --filter name=web-{u}").strip():
                break
            time.sleep(3)
        time.sleep(10)
        st = sql(f"SELECT v.uuid FROM local_persistent_volumes v JOIN service_applications a ON a.id=v.resource_id AND v.resource_type LIKE '%ServiceApplication' JOIN services s ON s.id=a.service_id WHERE s.uuid='{u}' LIMIT 1")
        path = f"/services/{u}/storages/{st}/backups"
        c, b = call(side, "PUT", path, {"frequency": "daily", "stop_during_backup": True})
        record(f"[stop] {side} schedule accepted", c in (200, 201), b)
        c, b = call(side, "POST", path + "/run")
        record(f"[stop] {side} run accepted", c in (200, 201, 202), b)
        query = ("SELECT row_to_json(r) FROM (SELECT e.*, b.uuid AS schedule_uuid FROM scheduled_volume_backup_executions e JOIN scheduled_volume_backups b ON b.id=e.scheduled_volume_backup_id "
                 f"JOIN local_persistent_volumes v ON v.id=b.backupable_id AND b.backupable_type LIKE '%LocalPersistentVolume' WHERE v.uuid='{st}' ORDER BY e.id DESC LIMIT 1) r")
        ex = wait_row(query)
        record(f"[stop] {side} backup finished", bool(ex) and ex["status"] == "success", ex)
        time.sleep(5)
        name = vm(f"docker ps --format '{{{{.Names}}}}' --filter name=web-{u}").strip()
        record(f"[stop] {side} container running after the backup", bool(name), name)
        seen[side] = {"status": ex and ex["status"], "pending": ex and ex["stop_recovery_pending"], "ids": ex and ex["stop_container_ids"], "attention": ex and ex["recovery_needs_attention"]}
        # Retry recovery: the state a crash during the archive leaves.
        # stateFile(): the container ids a crashed backup leaves on the server; recovery restarts those.
        container = vm(f"docker inspect --format '{{{{.Id}}}}' {name}").strip()
        vm(f"docker stop {name} >/dev/null; echo {container} > /tmp/coolify-volume-backup-{ex['uuid']}.stopped")
        sql(f"UPDATE scheduled_volume_backup_executions SET stop_recovery_pending=true WHERE uuid='{ex['uuid']}'")
        if side == "rs":
            code, b = rs.post(f"/ui/volume-backups/service/{u}/{ex['schedule_uuid']}/executions/{ex['uuid']}/retry-recovery", {})
            record("[stop] rs retry accepted", code == 200, b[:300])
        else:
            out = tinker(f'App\\Jobs\\VolumeBackupRecoveryJob::recoverWithBounds(App\\Models\\ScheduledVolumeBackupExecution::where("uuid","{ex["uuid"]}")->first());echo "done";')
            record("[stop] ref retry ran", "done" in out, out[-300:])
        for _ in range(30):
            if vm(f"docker ps -q --filter name=web-{u}").strip():
                break
            time.sleep(2)
        record(f"[stop] {side} retry started the container", bool(vm(f"docker ps -q --filter name=web-{u}").strip()))
        record(f"[stop] {side} retry removed the state file", vm(f"test -e /tmp/coolify-volume-backup-{ex['uuid']}.stopped && echo left").strip() == "")
        vm(f"rm -f /tmp/coolify-volume-backup-{ex['uuid']}.stopped /tmp/coolify-volume-backup-{ex['uuid']}.stopped.remaining")
        after = wait_row(query)
        seen[f"{side}.after"] = {"pending": after["stop_recovery_pending"], "ids": after["stop_container_ids"], "attention": after["recovery_needs_attention"], "error": after["recovery_error"]}
    record("[stop] executions alike", seen.get("ref") == seen.get("rs"), (seen.get("ref"), seen.get("rs")))
    record("[stop] after retry alike", seen.get("ref.after") == seen.get("rs.after"), (seen.get("ref.after"), seen.get("rs.after")))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--case", default="standalone,volume,service,stop")
    a = ap.parse_args()
    e = Env()
    try:
        e.prepare()
        for case in a.case.split(","):
            try:
                {"standalone": case_standalone, "volume": case_volume, "service": case_service, "stop": case_stop}[case](e)
            except Exception as ex:  # noqa: BLE001
                record(f"[{case}] ran to the end", False, repr(ex))
    finally:
        e.cleanup()
    print(f"{sum(results)}/{len(results)} passed")
    sys.exit(0 if all(results) else 1)


main()
