#!/usr/bin/env python3
"""A short, controlled run of the one scheduler with real commands on the test VM.

The instance runs against its own new database on the dev Postgres, so it cannot see the rows of the
reference or of anyone else. Only these schedule keys are allowed (`COOLIFY_RS_SCHEDULER_ONLY`): one
task, one database backup and one volume backup on its own containers, the Kernel's jobs, and a second
server row marked as a build server, so `ServerManagerJob` only checks its connection. Docker cleanup
is not allowed, because it prunes the shared VM. CDN and notification traffic goes to a local fake.

Everything created is written to the state file before it is created and removed by name at the end;
`--cleanup <state>` finishes a run that crashed.

    python3 compat/diff/scheduler_window.py --port 18089 --minutes 3 --state /tmp/jobs-window.json

Exit status 1 when a check fails.
"""
import argparse
import http.server
import json
import os
import secrets
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEV_PG = "coolify-rs-dev-postgres-1"
REF_PG = "coolify-rs-reference-postgres-1"
REF_APP = "coolify-rs-reference-coolify-1"
VM = "coolify-rs-e2e"
REF_SERVER = "hml20fsj5eles0xfiqidq8j5"


def run(args, **kw):
    return subprocess.run(args, capture_output=True, text=True, **kw)


def vm(script):
    return run(["orb", "-m", VM, "-u", "root", "bash", "-c", script])


def dev_sql(db, sql):
    r = run(["docker", "exec", "-i", DEV_PG, "psql", "-U", "coolify_rs", "-d", db, "-v", "ON_ERROR_STOP=1", "-Atq"], input=sql)
    if r.returncode != 0:
        raise SystemExit(f"sql failed: {r.stderr[:400]}")
    return r.stdout.strip()


def ref_sql(sql):
    return run(["docker", "exec", REF_PG, "psql", "-U", "coolify", "-d", "coolify", "-Atc", sql]).stdout.strip()


def cleanup(state):
    """Removes exactly what the state file names."""
    if state.get("pid"):
        try:
            os.kill(state["pid"], 15)
            time.sleep(1)
        except ProcessLookupError:
            pass
    for name in state.get("containers", []):
        vm(f"docker rm -f {name} >/dev/null 2>&1")
    for name in state.get("volumes", []):
        vm(f"docker volume rm -f {name} >/dev/null 2>&1")
    for path in state.get("paths", []):
        if path.startswith("/data/coolify/backups/") and "jobs-sched-" in path:
            vm(f"rm -rf {path}")
    if state.get("data_dir") and "jobs-sched-" in state["data_dir"]:
        shutil.rmtree(state["data_dir"], ignore_errors=True)
    if state.get("database"):
        run(["docker", "exec", DEV_PG, "dropdb", "-U", "coolify_rs", "--force", "--if-exists", state["database"]])


class Fake(http.server.BaseHTTPRequestHandler):
    seen = []
    answers = {}

    def _answer(self):
        length = int(self.headers.get("content-length") or 0)
        body = self.rfile.read(length) if length else b""
        Fake.seen.append((self.command, self.path, body.decode("utf8", "replace")))
        text = Fake.answers.get(self.path, "{}").encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(text)))
        self.end_headers()
        self.wfile.write(text)

    do_GET = do_POST = _answer

    def log_message(self, *_):
        pass


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=18089)
    ap.add_argument("--minutes", type=int, default=3)
    ap.add_argument("--state", required=True)
    ap.add_argument("--cleanup", action="store_true")
    a = ap.parse_args()
    state_path = Path(a.state)
    if a.cleanup:
        cleanup(json.loads(state_path.read_text()))
        return 0

    tag = secrets.token_hex(4)
    name = f"jobs-sched-{tag}"
    db = f"jobs_sched_{tag}"
    app_uuid, pg_uuid = f"{name}-app", f"{name}-pg"
    volume = f"{name}-vol"
    data_dir = Path(a.state).parent / f"{name}-data"
    state = {
        "database": db,
        "containers": [app_uuid, pg_uuid],
        "volumes": [volume],
        "paths": [
            f"/data/coolify/backups/databases/{name}-1",
            f"/data/coolify/backups/volumes/{name}-1",
        ],
        "data_dir": str(data_dir),
    }
    state_path.write_text(json.dumps(state))
    failures = []

    def check(label, ok, detail=""):
        print(("ok   " if ok else "FAIL ") + label + (f": {detail}" if detail and not ok else ""))
        if not ok:
            failures.append(label)

    try:
        fake = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fake)
        threading.Thread(target=fake.serve_forever, daemon=True).start()
        fake_url = f"http://127.0.0.1:{fake.server_address[1]}"
        Fake.answers["/versions.json"] = json.dumps({"coolify": {"v4": {"version": "99.0.0"}, "helper": {"version": "1.0.19"}, "sentinel": {"version": "0.0.1"}}, "traefik": {}})
        Fake.answers["/releases.json"] = json.dumps([{"tag_name": "v99", "name": "Ninety nine", "body": "notes", "draft": False, "published_at": "2026-10-05T10:00:00Z"}])
        Fake.answers["/templates.json"] = json.dumps({"jobs-window-template": {"documentation": "x", "slogan": "y", "compose": "c"}})

        # The database: the schema, the fixtures, the key of the test server (still encrypted).
        run(["docker", "exec", DEV_PG, "createdb", "-U", "coolify_rs", db], check=True)
        dev_sql(db, (ROOT / "compat/baseline/upstream-4.5.3-schema.sql").read_text())
        dev_sql(db, "SET search_path TO public;\n" + (ROOT / "compat/baseline/upstream-4.5.3-migrations.sql").read_text())
        ip, user, port = ref_sql(f"SELECT ip, \"user\", port FROM servers WHERE uuid = '{REF_SERVER}'").split("|")
        key = ref_sql(f"SELECT k.private_key FROM private_keys k JOIN servers s ON s.private_key_id = k.id WHERE s.uuid = '{REF_SERVER}'")
        app_key = run(["docker", "exec", REF_APP, "printenv", "APP_KEY"]).stdout.strip()
        enc_url = run(
            ["docker", "exec", REF_APP, "php", "/var/www/html/artisan", "tinker", "--execute", f"echo encrypt('{fake_url}/webhook', false);"]
        ).stdout.strip()
        dev_sql(db, f"""
SET search_path TO public;
INSERT INTO instance_settings (id, update_check_frequency, instance_timezone, webhook_allow_localhost, webhook_allowed_internal_hosts)
  VALUES (0, '* * * * *', 'UTC', true, '["127.0.0.1"]');
INSERT INTO teams (id, name, personal_team) VALUES (0, 'root', false), (1, '{name}', false);
INSERT INTO users (id, name, email) VALUES (1, 'a', 'a@{name}.test');
INSERT INTO team_user (team_id, user_id, role) VALUES (1, 1, 'owner');
INSERT INTO private_keys (id, uuid, name, private_key, team_id) VALUES (1, 'k-{tag}', 'k', '{key}', 1);
INSERT INTO servers (id, uuid, name, ip, "user", port, team_id, private_key_id, proxy) VALUES
  (1, 's-{tag}', 'vm', '{ip}', '{user}', {port}, 1, 1, '{{"type":"NONE"}}'),
  (2, 'b-{tag}', 'build', '{ip}', '{user}', {port}, 1, 1, '{{"type":"NONE"}}');
INSERT INTO server_settings (server_id, is_reachable, is_usable, is_build_server, docker_cleanup_frequency, patch_check_frequency) VALUES
  (1, true, true, false, '0 0 1 1 *', '0 0 1 1 *'), (2, true, true, true, '0 0 1 1 *', '0 0 1 1 *');
INSERT INTO standalone_dockers (id, uuid, name, network, server_id) VALUES (1, 'd-{tag}', 'd', 'bridge', 1);
INSERT INTO projects (id, uuid, name, team_id) VALUES (1, 'p-{tag}', 'p', 1);
INSERT INTO environments (id, uuid, name, project_id) VALUES (1, 'e-{tag}', 'production', 1);
INSERT INTO applications (id, uuid, name, environment_id, build_pack, git_branch, git_repository, status, destination_type, destination_id)
  VALUES (1, '{app_uuid}', 'app', 1, 'dockerimage', 'main', 'r', 'running:healthy', 'App\\Models\\StandaloneDocker', 1);
INSERT INTO application_settings (application_id) VALUES (1);
INSERT INTO standalone_postgresqls (id, uuid, name, postgres_user, postgres_password, postgres_db, environment_id, destination_type, destination_id, status, image)
  VALUES (1, '{pg_uuid}', 'pg', 'postgres', '{key}', 'postgres', 1, 'App\\Models\\StandaloneDocker', 1, 'running:healthy', 'postgres:16-alpine');
INSERT INTO local_persistent_volumes (id, uuid, name, mount_path, resource_type, resource_id)
  VALUES (1, 'v-{tag}', '{volume}', '/data', 'App\\Models\\Application', 1);
INSERT INTO scheduled_tasks (id, uuid, name, command, frequency, enabled, team_id, application_id)
  VALUES (1, 't-{tag}', 'hello', 'echo window-{tag}', '* * * * *', true, 1, 1);
INSERT INTO scheduled_database_backups (id, uuid, team_id, frequency, enabled, database_id, database_type, databases_to_backup, save_s3, disable_local_backup)
  VALUES (1, 'db-{tag}', 1, '* * * * *', true, 1, 'App\\Models\\StandalonePostgresql', 'postgres', false, false);
INSERT INTO scheduled_volume_backups (id, uuid, team_id, frequency, enabled, backupable_type, backupable_id)
  VALUES (1, 'vb-{tag}', 1, '* * * * *', true, 'App\\Models\\LocalPersistentVolume', 1);
INSERT INTO webhook_notification_settings (team_id, webhook_enabled, webhook_url, scheduled_task_success_webhook_notifications, backup_success_webhook_notifications)
  VALUES (1, true, '{enc_url}', true, true);
""")
        # The containers on the VM, under names only this run uses.
        pg_password = secrets.token_hex(8)
        started = vm(
            f"docker volume create {volume} >/dev/null && "
            f"docker run -d --name {app_uuid} --label coolify.managed=true --label coolify.applicationId=1 "
            f"--label coolify.applicationUuid={app_uuid} --label com.docker.compose.service=app "
            f"-v {volume}:/data alpine:latest sleep 900 >/dev/null && "
            f"docker run -d --name {pg_uuid} --label coolify.managed=true --label coolify.databaseUuid={pg_uuid} "
            f"-e POSTGRES_PASSWORD={pg_password} postgres:16-alpine >/dev/null"
        )
        if started.returncode != 0:
            raise SystemExit(f"containers did not start: {started.stderr[:300]}")
        time.sleep(8)

        # The instance.
        run(["cargo", "build", "-q", "-p", "coolify-rs"], cwd=ROOT, check=True)
        data_dir.mkdir(parents=True, exist_ok=True)
        only = ",".join(
            ["scheduled-task:1", "scheduled-backup:1", "scheduled-volume-backup:1", "server:2"]
            + [f"kernel:{k}" for k in ("ServerManagerJob", "CleanupInstanceStuffsJob", "PullTemplatesFromCDN", "PullChangelog", "CheckForUpdatesJob")]
        )
        env = dict(os.environ)
        env.update({
            "APP_KEY": app_key,
            "DATABASE_URL": f"postgres://coolify_rs:coolify_rs@127.0.0.1:25432/{db}",
            "COOLIFY_RS_LISTEN": f"127.0.0.1:{a.port}",
            "COOLIFY_RS_UI_DIR": str(ROOT / "frontend/dist"),
            "COOLIFY_RS_DATA_DIR": str(data_dir),
            "COOLIFY_RS_SKIP_STARTUP_CLEANUP": "1",
            "COOLIFY_RS_SCHEDULER": "on",
            "COOLIFY_RS_SCHEDULER_ONLY": only,
            "COOLIFY_RS_SSH_BINARY": str(ROOT / "scripts/ssh-via-orb.sh"),
            "COOLIFY_RS_TEMPLATES_URL": f"{fake_url}/templates.json",
            "COOLIFY_RS_TEMPLATES_DIR": str(data_dir),
            "RELEASES_URL": f"{fake_url}/releases.json",
            "VERSIONS_URL": f"{fake_url}/versions.json",
            "APP_ID": "jobs-window",
            "RUST_LOG": "info,sqlx=warn",
        })
        log = open(data_dir / "server.log", "w")
        server = subprocess.Popen([str(ROOT / "target/debug/coolify-rs"), "serve"], env=env, stdout=log, stderr=log)
        state["pid"] = server.pid
        state_path.write_text(json.dumps(state))
        print(f"window: {a.minutes} minutes, keys: {only}")
        time.sleep(a.minutes * 60 + 20)
        server.terminate()
        server.wait(timeout=20)
        state.pop("pid")
        state_path.write_text(json.dumps(state))

        # What ran.
        count = lambda sql: int(dev_sql(db, sql) or 0)
        tasks = dev_sql(db, "SELECT status || ':' || coalesce(message,'') FROM scheduled_task_executions ORDER BY id").splitlines()
        check(f"tasks ran once per minute ({len(tasks)} runs)", a.minutes - 1 <= len(tasks) <= a.minutes + 1, str(tasks))
        check("task output on the VM", tasks and all(t == f"success:window-{tag}" for t in tasks), str(tasks[:3]))
        backups = dev_sql(db, "SELECT status, filename FROM scheduled_database_backup_executions ORDER BY id").splitlines()
        check(f"database backups ({len(backups)} runs)", bool(backups) and all(b.startswith("success|") for b in backups[:-1]) and backups[-1].split("|")[0] in ("success", "running"), str(backups[-2:]))
        if backups:
            path = backups[0].split("|")[1]
            check("backup file on the VM", vm(f"test -s {path}").returncode == 0, path)
        volumes = dev_sql(db, "SELECT status, filename FROM scheduled_volume_backup_executions ORDER BY id").splitlines()
        check(f"volume backups ({len(volumes)} runs)", bool(volumes) and all(v.startswith("success|") for v in volumes[:-1]) and volumes[-1].split("|")[0] in ("success", "running"), str(volumes[-2:]))
        statuses = dev_sql(db, "SELECT job_type, status, count(*) FROM scheduled_job_deliveries GROUP BY 1, 2 ORDER BY 1, 2")
        print("deliveries:", statuses.replace("\n", ", "))
        check(
            "every occurrence finished or skipped by its overlap lock",
            count("SELECT count(*) FROM scheduled_job_deliveries WHERE status NOT IN ('completed', 'skipped', 'claimed')") == 0,
            statuses,
        )
        check(
            "one delivery per occurrence",
            count("SELECT count(*) FROM (SELECT 1 FROM scheduled_job_deliveries GROUP BY schedule_key, scheduled_for HAVING count(*) > 1) x") == 0,
        )
        check("docker cleanup stayed off", count("SELECT count(*) FROM docker_cleanup_executions") == 0)
        check("no other server was touched", count("SELECT count(*) FROM scheduled_job_deliveries WHERE job_type = 'docker-cleanup'") == 0)
        check("server 2 still reachable after the connection check", count("SELECT count(*) FROM server_settings WHERE server_id = 2 AND is_reachable") == 1)

        # CDN jobs.
        versions = json.loads((data_dir / "versions.json").read_text()) if (data_dir / "versions.json").exists() else {}
        check("versions.json from the fake", versions.get("coolify", {}).get("v4", {}).get("version") == "99.0.0")
        check("new_version_available", dev_sql(db, "SELECT new_version_available FROM instance_settings WHERE id = 0") == "t")
        check("changelog from the fake", (data_dir / "changelogs/2026-10.json").exists())
        check("templates from the fake", "jobs-window-template" in (data_dir / "service-templates-latest.json").read_text() if (data_dir / "service-templates-latest.json").exists() else False)

        # Notifications through the fake channel.
        hooks = [json.loads(b) for (m, p, b) in Fake.seen if p == "/webhook" and b]
        events = sorted({h.get("event") for h in hooks})
        print("webhook events:", events)
        check("task_success reached the fake", "task_success" in events, str(events))
        check("backup_success reached the fake", "backup_success" in events, str(events))
        check("volume backup notification reached the fake", any("volume" in str(e) for e in events), str(events))

        # The heartbeat.
        text = (data_dir / "server.log").read_text()
        ticks = text.count("ScheduledJobManager completed")
        check(f"the scheduler ticked ({ticks})", a.minutes - 1 <= ticks <= a.minutes + 1)
    finally:
        cleanup(state)
        print("cleaned up: database", db, "containers", state["containers"], "volumes", state["volumes"], "paths", state["paths"])
    print(f"{'FAILED ' + str(len(failures)) if failures else 'all checks passed'}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
