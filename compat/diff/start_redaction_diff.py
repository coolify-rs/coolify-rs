#!/usr/bin/env python3
"""Fetched secret values in the start logs of a database and a service, on both sides.

The fake Doppler returns IMG_SECRET. The resource's image name holds that value and points at a closed
port (localhost:1), so `docker compose` prints the value and the pull fails without contacting anything.
The reference runs under tinker with Http::fake and the sync queue, so no worker talks to a provider.

    python3 compat/diff/fake_providers.py 18191 &
    COOLIFY_RS_TEST_ONLY_FAKE_PROVIDERS=http://127.0.0.1:18191 scripts/dev-server.sh 18087
    COOLIFY_RS_BASE=http://127.0.0.1:18087 python3 compat/diff/start_redaction_diff.py
"""
import base64
import json
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request

from integrations_diff import ENVIRONMENT, PROJECT, RS, SERVER, TOKENS, compare, created, cleanup, ref_create_token, record, results, sql, tinker

SECRET = "dns2secretvalue99"
IMAGE = f"localhost:1/{SECRET}:1"
activities = []
resources = []


def api(side, method, path, body=None):
    base = "http://127.0.0.1:18000" if side == "ref" else RS
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(base + "/api/v1" + path, data=data, method=method)
    req.add_header("Authorization", "Bearer " + TOKENS["acme_owner_root"])
    req.add_header("Content-Type", "application/json")
    req.add_header("Accept", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            return r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")


def link(morph, rid, tid):
    sql(f"INSERT INTO secret_manager_links (uuid, resourceable_type, resourceable_id, integration_token_id, settings, created_at, updated_at) "
        f"VALUES (md5(random()::text), '{morph}', {rid}, {tid}, '{{}}', now(), now())")


def last_activity():
    return int(sql("SELECT coalesce(max(id), 0) FROM activity_log") or 0)


def wait_activity(after, needle):
    """The first activity after `after` that belongs to this resource and has ended."""
    for _ in range(60):
        row = sql(f"SELECT id, properties->>'status', description FROM activity_log WHERE id > {after} AND "
                  f"properties->>'type_uuid' = '{needle}' ORDER BY id LIMIT 1")
        if row and row.split("|")[1] in ("finished", "error", "failed"):
            aid = int(row.split("|")[0])
            activities.append(aid)
            return row.split("|", 2)[2]
        time.sleep(2)
    return "timeout"


def shape(description):
    """Whether the value shows, whether it was hidden, and each line without volatile text."""
    try:
        entries = json.loads(description)
    except ValueError:
        return {"unparsed": description[:300]}
    if not any("localhost:1" in e.get("output", "") for e in entries):
        print("  output:", [e.get("output", "")[:200] for e in entries][-4:])
    text = "".join(e.get("output", "") for e in entries)
    lines = [re.sub(r"\s+", " ", x).strip() for x in re.split(r"[\r\n]+", text)]
    keep = [x for x in lines if "localhost:1" in x or "REDACTED" in x or SECRET in x]
    return {"leaks": SECRET in text, "redacted": "REDACTED" in text, "lines": sorted(set(keep))[:10]}


def vm(cmd):
    return subprocess.run(["orb", "-m", "coolify-rs-e2e", "-u", "root", "bash", "-c", cmd], capture_output=True, text=True, timeout=120).stdout


def server_files(path, uuid):
    """The lines of the resource's compose and environment files on the server that hold the value."""
    dotenv = "." + "env"
    text = vm(f"cat /data/coolify/{path}/{uuid}/docker-compose.yml /data/coolify/{path}/{uuid}/{dotenv} 2>/dev/null")
    return sorted(x.strip() for x in text.splitlines() if SECRET in x and "image" not in x)


def ref_run(php):
    tinker("config(['queue.default' => 'sync']); try { " + php + " } catch (\\Throwable $e) { } echo '<<<null>>>';")


def main():
    tid = ref_create_token("doppler", "dns2-redaction", "dp.st.fake", ["secret_manager"], {"config": "x", "project": "y"})
    internal = tid["id"]
    try:
        code, db = api("ref", "POST", "/databases/postgresql", {
            "server_uuid": SERVER, "project_uuid": PROJECT, "environment_uuid": ENVIRONMENT,
            "image": IMAGE, "name": "dns2-redaction-db", "instant_deploy": False})
        assert code in (200, 201), db
        uuid = db["uuid"]
        rid = int(sql(f"SELECT id FROM standalone_postgresqls WHERE uuid = '{uuid}'"))
        resources.append(("db", rid, uuid))
        code, env = api("ref", "POST", f"/databases/{uuid}/envs", {"key": "FETCHED", "value": "{{vault.IMG_SECRET}}"})
        assert code in (200, 201), env
        link("App\\Models\\StandalonePostgresql", rid, internal)

        after = last_activity()
        ref_run(f"""$d = \\App\\Models\\StandalonePostgresql::find({rid});
\\Illuminate\\Support\\Facades\\Bus::fake([\\App\\Jobs\\DatabaseStartJob::class]);
\\App\\Actions\\Database\\StartDatabase::run($d);
$a = \\Spatie\\Activitylog\\Models\\Activity::query()->where('properties->type_uuid', $d->uuid)->latest('id')->first();
app()->call([new \\App\\Jobs\\DatabaseStartJob($d->getMorphClass(), (int) $d->id, (int) $d->team()->id, (int) $a->id, null), 'handle']);""")
        ref = shape(wait_activity(after, uuid))
        ref_file = server_files("databases", uuid)
        after = last_activity()
        code, body = api("rs", "POST", f"/databases/{uuid}/start")
        rs = shape(wait_activity(after, uuid))
        rs_file = server_files("databases", uuid)
        record("database start: fetched value reached the server", bool(ref_file), ref_file)
        compare("database start: fetched value on the server", ref_file, rs_file)
        print("rs start", code, body)
        record("database start: value hidden on the reference", ref.get("leaks") is False and ref.get("redacted") is True, ref)
        record("database start: value hidden on coolify-rs", rs.get("leaks") is False and rs.get("redacted") is True, rs)
        compare("database start: lines", ref.get("lines"), rs.get("lines"))

        compose = f"services:\n  web:\n    image: {IMAGE}\n    environment:\n      - FETCHED=${{FETCHED}}\n"
        code, svc = api("ref", "POST", "/services", {
            "server_uuid": SERVER, "project_uuid": PROJECT, "environment_uuid": ENVIRONMENT, "name": "dns2-redaction-svc",
            "docker_compose_raw": base64.b64encode(compose.encode()).decode(), "instant_deploy": False})
        assert code in (200, 201), svc
        suuid = svc["uuid"]
        sid = int(sql(f"SELECT id FROM services WHERE uuid = '{suuid}'"))
        resources.append(("svc", sid, suuid))
        sql(f"DELETE FROM environment_variables WHERE resourceable_type = 'App\\Models\\Service' AND resourceable_id = {sid} AND key = 'FETCHED'")
        code, env = api("ref", "POST", f"/services/{suuid}/envs", {"key": "FETCHED", "value": "{{vault.IMG_SECRET}}"})
        assert code in (200, 201), env
        link("App\\Models\\Service", sid, internal)

        after = last_activity()
        ref_run(f"\\App\\Actions\\Service\\StartService::run(\\App\\Models\\Service::find({sid}));")
        ref = shape(wait_activity(after, suuid))
        ref_file = server_files("services", suuid)
        after = last_activity()
        code, body = api("rs", "POST", f"/services/{suuid}/start")
        rs = shape(wait_activity(after, suuid))
        rs_file = server_files("services", suuid)
        record("service start: fetched value reached the server", bool(ref_file), ref_file)
        compare("service start: fetched value on the server", ref_file, rs_file)
        print("rs start", code, body)
        record("service start: value hidden on the reference", ref.get("leaks") is False and ref.get("redacted") is True, ref)
        record("service start: value hidden on coolify-rs", rs.get("leaks") is False and rs.get("redacted") is True, rs)
        compare("service start: lines", ref.get("lines"), rs.get("lines"))
    finally:
        for kind, rid, uuid in resources:
            path = "databases" if kind == "db" else "services"
            morph = "StandalonePostgresql" if kind == "db" else "Service"
            sql(f"DELETE FROM secret_manager_links WHERE resourceable_type = 'App\\Models\\{morph}' AND resourceable_id = {rid}")
            api("ref", "DELETE", f"/{path}/{uuid}?delete_configurations=true&delete_volumes=true&docker_cleanup=false&delete_connected_networks=false")
        for aid in activities:
            sql(f"DELETE FROM activity_log WHERE id = {aid}")
        time.sleep(5)
        cleanup()
    bad = [n for n, ok in results if not ok]
    print(f"{len(results) - len(bad)}/{len(results)} same")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
