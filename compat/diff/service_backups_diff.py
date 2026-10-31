#!/usr/bin/env python3
"""Differential check of the backup routes outside standalone databases.

- PUT, DELETE and POST .../run on /applications/{uuid}/storages/{storage_uuid}/backups and
  /services/{uuid}/storages/{storage_uuid}/backups, with real runs on the test VM.
- POST .../imports/uploads, POST .../imports and GET .../imports/{id} on /services/{uuid}/databases/{uuid}.
- A real DatabaseBackupJob of a service database, restored again through the import route.
- The "S3 endpoint is not allowed" message: an S3 row with a private endpoint, which both sides refuse before any
  request. `instance_settings` stays untouched, so no upload runs.

Each side gets its own service and application. Everything created is deleted by uuid or id, including the backup
folders of the run. Usage:

    COOLIFY_RS_BASE=http://127.0.0.1:18087 compat/diff/service_backups_diff.py
"""
import http.cookiejar
import json
import os
import re
import signal
import subprocess
import sys
import tempfile
import time
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import databases_diff as d  # noqa: E402

SERVER = "hml20fsj5eles0xfiqidq8j5"
PROJECT = "j8xm2emlllhfnyebjzgyzp7z"
ENV = "uj3st9yvtvzxh7rul3c3opcz"
ROOT = "acme_owner_root"
LOGIN = os.path.join(d.ROOT, "compat/reference/secrets/ui-login.txt")
COMPOSE = """services:
  db:
    image: postgres:16
    environment:
      POSTGRES_PASSWORD: wavefive
      POSTGRES_DB: wavedb
      POSTGRES_USER: waveuser
    volumes:
      - dbdata:/var/lib/postgresql/data
  web:
    image: nginx:latest
    volumes:
      - webdata:/data
"""
failed = []
# `timeout` and Ctrl-C still run the cleanup in `finally`.
signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
ANSWER_ONLY = "--api-only" in sys.argv


def vm(cmd):
    return subprocess.run(["orb", "-m", "coolify-rs-e2e", "-u", "root", "bash", "-c", cmd], capture_output=True, text=True).stdout.strip()


def tinker(code):
    return subprocess.run(["docker", "exec", "coolify-rs-reference-coolify-1", "php", "/var/www/html/artisan", "tinker", "--execute", code], capture_output=True, text=True).stdout.strip()


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
    text = re.sub(r'"uuid":"[a-z0-9]{20,}"', '"uuid":"U"', text)
    text = re.sub(r"http://[0-9a-z.:]+/settings/advanced", "URL/settings/advanced", text)
    try:
        return json.loads(text)
    except ValueError:
        return text


def row(q):
    t = d.sql(f"select row_to_json(x) from ({q}) x")
    return json.loads(t) if t else None


def schedule_row(backup):
    r = row(f"select frequency, enabled, save_s3, disable_local_backup, stop_during_backup, backupable_type, retention_amount_locally, retention_days_locally, retention_max_storage_locally, retention_amount_s3, retention_days_s3, retention_max_storage_s3, timeout, missing_backup_notification_days, team_id from scheduled_volume_backups where uuid = '{backup}'")
    return r


SEEN = {}


def wait_execution(backup, _count=None, table="scheduled_volume_backup_executions", parent="scheduled_volume_backups", key="scheduled_volume_backup_id", timeout=150):
    """The first finished execution newer than the last one seen: retention removes old rows, so counts do not work."""
    end = time.time() + timeout
    after = SEEN.get(backup, 0)
    q = f"select e.id, e.status, e.message, e.filename, e.size, e.s3_uploaded, e.local_storage_deleted from {table} e join {parent} b on b.id = e.{key} where b.uuid = '{backup}' and e.id > {after} order by e.id limit 1"
    while time.time() < end:
        r = row(q)
        if r and r["status"] != "running":
            SEEN[backup] = r.pop("id")
            return r
        time.sleep(2)
    print("timeout waiting for", backup, flush=True)
    return {"status": "timeout", "s3_uploaded": None}


def stamp(name):
    return re.sub(r"-\d{9,}", "-T", name or "")


class Rs:
    """A session on the coolify-rs instance, for what has no API route."""

    def __init__(self):
        self.jar = http.cookiejar.CookieJar()
        self.open = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(self.jar))
        self.base = d.BASES["rs"]
        password = re.search(r"^password:\s*(.+)$", open(LOGIN).read(), re.M).group(1).strip()
        self.csrf = ""
        c, b = self.post("/ui/session", {"email": "alice@example.test", "password": password})
        self.csrf = json.loads(b).get("csrf_token", "")

    def post(self, path, body):
        r = urllib.request.Request(self.base + path, data=json.dumps(body).encode(), method="POST")
        r.add_header("Content-Type", "application/json")
        r.add_header("Accept", "application/json")
        r.add_header("X-CSRF-Token", self.csrf)
        try:
            with self.open.open(r) as resp:
                return resp.status, resp.read()
        except urllib.error.HTTPError as e:
            return e.code, e.read()


def resource_matrix(side, kind, uuid, storage, subs, out, label):
    """The answers of the shared controller for one resource and one storage."""
    root = f"/api/v1/{kind}"
    path = f"{root}/{uuid}/storages/{storage}/backups"

    def req(name, method, p, body=None, tok=ROOT):
        c, b = d.call(side, method, p, body, tok=tok)
        out[f"{label}.{name}"] = [c, norm(b, subs)]
        return c, b

    req("no_resource", "PUT", f"{root}/nope/storages/{storage}/backups", {"frequency": "daily"})
    req("no_storage", "PUT", f"{root}/{uuid}/storages/nope/backups", {"frequency": "daily"})
    req("run_none", "POST", path + "/run")
    req("del_none", "DELETE", path)
    req("del_no_resource", "DELETE", f"{root}/nope/storages/{storage}/backups")
    req("run_no_resource", "POST", f"{root}/nope/storages/{storage}/backups/run")
    req("read_token", "PUT", path, {"frequency": "daily"}, tok="acme_owner_read")
    req("run_write_token", "POST", path + "/run", tok="acme_owner_write")
    req("member_token", "PUT", path, {"frequency": "daily"}, tok="acme_member_write_legacy")
    req("other_team", "PUT", path, {"frequency": "daily"}, tok="carol_root")
    req("stop_no_deploy", "PUT", path, {"frequency": "daily", "stop_during_backup": True}, tok="acme_owner_write")
    req("required", "PUT", path, {})
    req("bad_cron", "PUT", path, {"frequency": "nope"})
    req("extra", "PUT", path, {"frequency": "daily", "other": 1})
    req("types", "PUT", path, {"frequency": 5, "enabled": "x", "save_s3": "x", "retention_amount_locally": "a", "retention_days_locally": -1, "retention_max_storage_locally": "a", "retention_amount_s3": 20000, "timeout": 5, "missing_backup_notification_days": 400, "s3_storage_uuids": "x", "s3_storage_uuid": 5})
    req("s3_missing", "PUT", path, {"frequency": "daily", "save_s3": True})
    req("s3_unknown", "PUT", path, {"frequency": "daily", "save_s3": True, "s3_storage_uuid": "nope"})
    c, b = req("create", "PUT", path, {"frequency": "daily", "retention_amount_locally": 3, "retention_max_storage_locally": 1.5})
    backup = json.loads(b)["uuid"]
    out[f"{label}.row_create"] = schedule_row(backup)
    c, b = req("update", "PUT", path, {"frequency": "0 2 * * *", "enabled": False, "timeout": 600, "missing_backup_notification_days": 3})
    out[f"{label}.row_update"] = schedule_row(backup)
    out[f"{label}.same_uuid"] = json.loads(b)["uuid"] == backup
    return path, backup


def run_side(side, s3_uuid):
    out = {}
    track = {"dirs": []}
    subs = {}
    c, b = d.call(side, "POST", "/api/v1/services", {"server_uuid": SERVER, "project_uuid": PROJECT, "environment_uuid": ENV, "name": f"wave5-{side}", "docker_compose_raw": d.b64(COMPOSE), "instant_deploy": False}, tok=ROOT)
    if c >= 300:
        raise SystemExit(f"{side}: service create {c} {b[:300]}")
    service = json.loads(b)["uuid"]
    c, b = d.call(side, "POST", "/api/v1/applications/dockerimage", {"server_uuid": SERVER, "project_uuid": PROJECT, "environment_uuid": ENV, "name": f"wave5app-{side}", "docker_registry_image_name": "nginx", "docker_registry_image_tag": "latest", "ports_exposes": "80", "instant_deploy": False}, tok=ROOT)
    if c >= 300:
        d.call(side, "DELETE", f"/api/v1/services/{service}?delete_volumes=true&delete_configurations=true&docker_cleanup=true", tok=ROOT)
        raise SystemExit(f"{side}: application create {c} {b[:300]}")
    app = json.loads(b)["uuid"]
    subs.update({service: "SERVICE", app: "APP", f"wave5-{side}": "NAME", f"wave5app-{side}": "APPNAME"})
    backups = []
    try:
        database = row(f"select d.id, d.uuid, d.name from service_databases d join services s on s.id = d.service_id where s.uuid = '{service}' and d.name = 'db'")
        webapp = row(f"select a.id, a.uuid from service_applications a join services s on s.id = a.service_id where s.uuid = '{service}' and a.name = 'web'")
        db_volume = row(f"select uuid, name from local_persistent_volumes where resource_type = 'App\\Models\\ServiceDatabase' and resource_id = {database['id']}")
        web_volume = row(f"select uuid, name from local_persistent_volumes where resource_type = 'App\\Models\\ServiceApplication' and resource_id = {webapp['id']}")
        subs.update({database["uuid"]: "DBUUID", webapp["uuid"]: "WEBUUID", db_volume["uuid"]: "DBVOL", web_volume["uuid"]: "WEBVOL"})
        # The application storage API is on another branch, so the row goes in directly on both sides.
        volume_uuid = f"wave5vol{side}{int(time.time())}"
        d.sql(
            "insert into local_persistent_volumes (uuid, name, mount_path, resource_type, resource_id, created_at, updated_at) "
            f"select '{volume_uuid}', 'wave5vol-{side}', '/data', 'App\\Models\\Application', a.id, now(), now() from applications a where a.uuid = '{app}'"
        )
        app_volume = row(f"select v.uuid from local_persistent_volumes v join applications a on a.id = v.resource_id where v.resource_type = 'App\\Models\\Application' and a.uuid = '{app}'")
        subs.update({app_volume["uuid"]: "APPVOL", f"wave5vol-{side}": "APPVOLNAME"})
        team = row("select t.id, t.name from teams t join projects p on p.team_id = t.id where p.uuid = '" + PROJECT + "'")
        slug = re.sub(r"[^a-z0-9]+", "-", team["name"].lower()).strip("-")
        track["dirs"] = [
            f"/data/coolify/backups/volumes/{slug}-{team['id']}/{v}" for v in (db_volume["uuid"], web_volume["uuid"], app_volume["uuid"])
        ] + [f"/data/coolify/backups/databases/{slug}-{team['id']}/wave5-{side}-db-{service}"]

        path_app, b_app = resource_matrix(side, "applications", app, app_volume["uuid"], subs, out, "app")
        backups.append(b_app)
        path_web, b_web = resource_matrix(side, "services", service, web_volume["uuid"], subs, out, "svc")
        backups.append(b_web)

        # Service import API without a running database.
        imp = f"/api/v1/services/{service}/databases/{database['uuid']}/imports"

        def req(name, method, p, body=None, tok=ROOT):
            c, b = d.call(side, method, p, body, tok=tok)
            out[f"imp.{name}"] = [c, norm(b, subs)]
            return c, b

        req("no_service", "POST", f"/api/v1/services/nope/databases/{database['uuid']}/imports", {"source": "server", "path": "/a"})
        req("no_db", "POST", f"/api/v1/services/{service}/databases/nope/imports", {"source": "server", "path": "/a"})
        req("foreign_db", "POST", f"/api/v1/services/{service}/databases/{webapp['uuid']}/imports", {"source": "server", "path": "/a"})
        req("required", "POST", imp, {})
        req("bad_source", "POST", imp, {"source": "x"})
        req("server_no_path", "POST", imp, {"source": "server"})
        req("upload_no_id", "POST", imp, {"source": "upload"})
        req("s3_no_uuid", "POST", imp, {"source": "s3", "path": "a.gz"})
        req("not_running", "POST", imp, {"source": "server", "path": "/tmp/nothing.dump"})
        req("read_token", "POST", imp, {"source": "server", "path": "/a"}, tok="acme_owner_read")
        req("no_deploy", "POST", imp, {"source": "server", "path": "/a"}, tok="acme_owner_write")
        req("member", "POST", imp, {"source": "server", "path": "/a"}, tok="acme_member_write_legacy")
        req("show_missing", "GET", imp + "/999999999")
        req("show_not_number", "GET", imp + "/abc")
        req("show_no_service", "GET", f"/api/v1/services/nope/databases/{database['uuid']}/imports/1")
        req("upload_read_token", "POST", imp + "/uploads", {}, tok="acme_owner_read")
        if ANSWER_ONLY:
            return out

        # A volume that does not exist on the server.
        d.call(side, "POST", path_app + "/run", tok=ROOT)
        ex = wait_execution(b_app, 1)
        out["app.no_volume"] = {**ex, "message": norm(ex.get("message") or "", subs), "size": int(ex.get("size") or 0) > 0}
        # A real run of a stopped application's volume.
        vm(f"docker volume create wave5vol-{side}")
        track["volume"] = f"wave5vol-{side}"
        d.call(side, "POST", path_app + "/run", tok=ROOT)
        ex = wait_execution(b_app, 2)
        name = ex.get("filename") or ""
        out["app.execution"] = {**ex, "filename": stamp(norm(name, subs)), "size": int(ex.get("size") or 0) > 0}
        out["app.file_exists"] = vm(f"test -s '{name}' && echo yes || echo no")

        # S3 refused before any request: the message has the absolute settings URL.
        c, b = d.call(side, "PUT", path_app, {"frequency": "daily", "save_s3": True, "s3_storage_uuid": s3_uuid}, tok=ROOT)
        out["app.s3_set"] = [c, norm(b, {**subs, s3_uuid: "S3"})]
        c, b = d.call(side, "POST", path_app + "/run", tok=ROOT)
        out["app.s3_run"] = [c, norm(b, subs)]
        ex = wait_execution(b_app, 3, timeout=120)
        out["app.s3_refused"] = {"status": ex["status"], "s3_uploaded": ex["s3_uploaded"], "message": norm(ex.get("message") or "", subs)}
        c, b = d.call(side, "DELETE", path_app, tok=ROOT)
        out["app.delete"] = [c, norm(b, subs)]
        out["app.file_gone"] = vm(f"test -e '{name}' && echo present || echo gone")

        # Start the stack.
        c, b = d.call(side, "POST", f"/api/v1/services/{service}/start", tok=ROOT)
        out["svc.start"] = c
        end = time.time() + 240
        while time.time() < end:
            st = vm(f"docker inspect --format '{{{{.State.Health.Status}}}}{{{{.State.Status}}}}' db-{service}")
            if st.endswith("running") and d.sql(f"select status from service_databases where uuid = '{database['uuid']}'").startswith("running"):
                break
            time.sleep(4)
        out["svc.running"] = d.sql(f"select status from service_databases where uuid = '{database['uuid']}'").split(":")[0]
        if out["svc.running"] != "running":
            print(side, "service did not start:", vm(f"docker ps -a --format '{{{{.Names}}}} {{{{.Status}}}}' | grep {service}")[:500])
            print(side, "network pool:", vm(f"docker network ls --filter name={service} -q | wc -l"))
            return out
        psql = f"docker exec db-{service} psql -U waveuser -d wavedb -Atc"
        vm(f"{psql} 'create table seed (id int); insert into seed values (7)'")

        # Volume backup of a running part, with containers kept running.
        d.call(side, "POST", path_web + "/run", tok=ROOT)
        ex = wait_execution(b_web, 1)
        name = ex.get("filename") or ""
        out["svc.execution"] = {**ex, "filename": stamp(norm(name, subs)), "size": int(ex.get("size") or 0) > 0}
        out["svc.file_exists"] = vm(f"test -s '{name}' && echo yes || echo no")

        # DatabaseBackupJob of the service database: the schedule row is inserted the way `CreateScheduledBackup` does.
        backup_uuid = f"wave5{side}{int(time.time())}"
        d.sql(
            "insert into scheduled_database_backups (uuid, team_id, enabled, frequency, save_s3, database_type, database_id, created_at, updated_at) "
            f"values ('{backup_uuid}', {team['id']}, true, 'daily', false, 'App\\Models\\ServiceDatabase', {database['id']}, now(), now())"
        )
        track["db_backup"] = backup_uuid
        if side == "ref":
            tinker(f"App\\Jobs\\DatabaseBackupJob::dispatch(App\\Models\\ScheduledDatabaseBackup::whereUuid('{backup_uuid}')->first());")
        else:
            c, b = Rs().post(f"/ui/services/{service}/backup-now", {"type": "database", "backup": backup_uuid})
            if c >= 300:
                print("backup-now answered", c, b[:200])
        ex = wait_execution(backup_uuid, 1, "scheduled_database_backup_executions", "scheduled_database_backups", "scheduled_database_backup_id")
        file = ex.get("filename") or ""
        extra = row(f"select e.database_name from scheduled_database_backup_executions e join scheduled_database_backups b on b.id = e.scheduled_database_backup_id where b.uuid = '{backup_uuid}' order by e.id limit 1") or {}
        out["db.execution"] = {**ex, "filename": re.sub(r"-[a-z0-9]{20,}\.dmp$", "-X.dmp", stamp(norm(file, subs))), "size": int(ex.get("size") or 0) > 0, "database_name": extra.get("database_name")}
        out["db.file_listing"] = sorted(re.findall(r"TABLE public \w+", vm(f"docker exec -i db-{service} pg_restore -l < '{file}'")))

        # Restore the dump into the database through the import route.
        vm(f"{psql} 'drop table seed'")
        c, b = d.call(side, "POST", imp, {"source": "server", "path": file}, tok=ROOT)
        out["imp.server_start"] = [c, re.sub(r'"id": ?\d+', '"id":N', re.sub(r"imports/\d+", "imports/N", norm(b, subs) if isinstance(norm(b, subs), str) else json.dumps(norm(b, subs))))]
        activity = json.loads(b).get("id") if c < 300 else None
        if activity:
            end = time.time() + 180
            while time.time() < end:
                c, b = d.call(side, "GET", f"{imp}/{activity}", tok=ROOT)
                body = json.loads(b)
                if body.get("status") in ("finished", "error", "failed"):
                    break
                time.sleep(3)
            out["imp.show"] = {k: body.get(k) for k in ("status", "exit_code")}
            out["imp.show_keys"] = list(body.keys())
            out["imp.restored"] = vm(f"{psql} 'select id from seed'")
            props = row(f"select properties->>'resource_kind' as kind, subject_type from activity_log where id = {activity}")
            out["imp.activity"] = props

        # An upload of a small dump through the service route.
        with tempfile.NamedTemporaryFile(suffix=".sql", delete=False) as f:
            f.write(b"create table if not exists uploaded (id int); insert into uploaded values (1);\n")
            local = f.name
        upload_id = "wave5upload" + side
        r = subprocess.run(["curl", "-s", "-w", "\n%{http_code}", "-H", f"Authorization: Bearer {d.TOKENS[ROOT]}", "-H", "Accept: application/json", "-F", f"upload_id={upload_id}", "-F", f"file=@{local};filename=b.sql", f"{d.BASES[side]}{imp}/uploads"], capture_output=True, text=True)
        os.unlink(local)
        body, code = r.stdout.rsplit("\n", 1)
        out["imp.upload"] = [code, norm(body, {**subs, upload_id: "UP"})]
        c, b = d.call(side, "POST", imp, {"source": "upload", "upload_id": upload_id}, tok=ROOT)
        out["imp.upload_start"] = c
        if c < 300:
            activity = json.loads(b)["id"]
            end = time.time() + 120
            while time.time() < end:
                c, b = d.call(side, "GET", f"{imp}/{activity}", tok=ROOT)
                body = json.loads(b)
                if body.get("status") in ("finished", "error", "failed"):
                    break
                time.sleep(3)
            out["imp.upload_status"] = body.get("status")
            out["imp.upload_restored"] = vm(f"{psql} 'select id from uploaded'")
    finally:
        if track.get("db_backup"):
            d.sql(f"delete from scheduled_database_backup_executions where scheduled_database_backup_id in (select id from scheduled_database_backups where uuid = '{track['db_backup']}')")
            d.sql(f"delete from scheduled_database_backups where uuid = '{track['db_backup']}'")
        for b_uuid in backups:
            d.sql(f"delete from scheduled_volume_backup_executions where scheduled_volume_backup_id in (select id from scheduled_volume_backups where uuid = '{b_uuid}')")
            d.sql(f"delete from scheduled_volume_backups where uuid = '{b_uuid}'")
        d.call(side, "DELETE", f"/api/v1/services/{service}?delete_volumes=true&delete_configurations=true&docker_cleanup=true", tok=ROOT)
        d.call(side, "DELETE", f"/api/v1/applications/{app}?delete_volumes=true&delete_configurations=true&docker_cleanup=true", tok=ROOT)
        if track.get("volume"):
            vm(f"docker volume rm {track['volume']}")
        for folder in track["dirs"]:
            vm(f"rm -rf '{folder}'")
    return out


stamp_s3 = str(int(time.time()))
s3_uuids = {"ref": "wave5s3ref" + stamp_s3, "rs": "wave5s3rs" + stamp_s3}
team_id = d.sql(f"select team_id from projects where uuid = '{PROJECT}'")
try:
    # One row per side: the refused connection test marks the storage unusable.
    for s3_uuid in s3_uuids.values():
        tinker(
            f"$s = new App\\Models\\S3Storage; $s->forceFill(['uuid' => '{s3_uuid}', 'name' => 'wave5-fake', 'region' => 'us-east-1', 'key' => 'wavekey', 'secret' => 'wavesecret', 'bucket' => 'wavebucket', 'endpoint' => 'http://127.0.0.1:9000', 'is_usable' => true, 'team_id' => {team_id}]); $s->save(); echo $s->id;"
        )
    ref = run_side("ref", s3_uuids["ref"])
    print("reference side done", flush=True)
    rs = run_side("rs", s3_uuids["rs"])
    print("coolify-rs side done", flush=True)
    for key in sorted(set(ref) | set(rs)):
        check(key, ref.get(key), rs.get(key))
finally:
    for s3_uuid in s3_uuids.values():
        d.sql(f"delete from volume_backup_s3_replicas where s3_storage_id in (select id from s3_storages where uuid = '{s3_uuid}')")
        d.sql(f"delete from scheduled_volume_backup_s3_storage where s3_storage_id in (select id from s3_storages where uuid = '{s3_uuid}')")
        d.sql(f"delete from s3_storages where uuid = '{s3_uuid}'")
print("failed:", failed)
sys.exit(1 if failed else 0)
