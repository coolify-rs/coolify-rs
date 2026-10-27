#!/usr/bin/env python3
"""Differential check of server validation: upstream reference vs coolify-rs.

Needs the reference environment (scripts/reference-up.sh), the SSH test host
(scripts/testing-host-up.sh, which gives it one network alias per scenario),
and coolify-rs serving on 127.0.0.1:18080 against the reference database with
remote execution enabled.

For each scenario user of the test host, one server is created through the
reference API. It is validated by the reference, the rows are recorded and
reset, then the same server is validated by coolify-rs (pointed at the
published port, since the aliases only resolve inside Docker), and the rows
are compared. Exit status 1 when any scenario differs.
"""
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
TOKENS = json.load(open(os.path.join(ROOT, "compat/reference/secrets/tokens.json")))
KEY = open(os.path.join(ROOT, "compat/testing-host/secrets/id_ed25519")).read()
REF = "http://127.0.0.1:18000"
RS = "http://127.0.0.1:18080"
RS_HOST, RS_PORT = "127.0.0.1", int(os.environ.get("COOLIFY_RS_TEST_SSH_PORT", "2223"))
# (user, alias): `ghost` does not exist on the host, so the login is refused.
USERS = [(u, u) for u in ["root", "deploy", "nojq", "nodocker", "olddocker", "badjson", "gentoo"]] + [("ghost", "root")]
FIELDS = "s.validation_logs, s.is_validating, ss.is_reachable, ss.is_usable, ss.docker_version, ss.compose_version, " \
         "ss.docker_version_checked_at IS NOT NULL, ss.compose_version_checked_at IS NOT NULL"


def api(base, method, path, body=None, token="acme_owner_write"):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(base + path, data=data, method=method)
    r.add_header("Authorization", "Bearer " + TOKENS[token])
    r.add_header("Accept", "application/json")
    if data:
        r.add_header("Content-Type", "application/json")
    for _ in range(5):
        try:
            resp = urllib.request.urlopen(r)
            return resp.status, json.loads(resp.read() or b"null")
        except urllib.error.HTTPError as e:
            if e.code == 429:
                time.sleep(int(e.headers.get("Retry-After", "5")))
                continue
            return e.code, json.loads(e.read() or b"null")
    raise SystemExit("rate limited")


def sql(query):
    out = subprocess.run(
        ["docker", "exec", "coolify-rs-reference-postgres-1", "psql", "-U", "coolify", "-d", "coolify", "-At", "-F", "\x1f", "-c", query],
        check=True, capture_output=True, text=True,
    ).stdout
    return [line.split("\x1f") for line in out.splitlines() if line]


def snapshot(uuid):
    return sql(f"SELECT {FIELDS} FROM servers s JOIN server_settings ss ON ss.server_id = s.id WHERE s.uuid = '{uuid}'")[0]


def reset(uuid):
    sql(f"UPDATE servers SET validation_logs = NULL, is_validating = false WHERE uuid = '{uuid}'")
    sql("UPDATE server_settings SET is_reachable = false, is_usable = false, docker_version = NULL, compose_version = NULL, "
        "docker_version_checked_at = NULL, compose_version_checked_at = NULL "
        f"WHERE server_id = (SELECT id FROM servers WHERE uuid = '{uuid}')")


def wait(uuid, timeout=90):
    """Until validation_logs is set, or the version checks ran and the last write landed."""
    end = time.time() + timeout
    while time.time() < end:
        logs, _, _, usable, _, _, _, compose_checked = snapshot(uuid)
        if logs or (compose_checked == "t" and usable == "t"):
            time.sleep(2)
            return snapshot(uuid)
        time.sleep(1)
    return snapshot(uuid) + ["<timeout>"]


def validate(base, uuid):
    code, body = api(base, "POST", f"/api/v1/servers/{uuid}/validate")
    if code != 201:
        raise SystemExit(f"{base} validate {uuid}: {code} {body}")
    return wait(uuid)


def main():
    code, body = api(REF, "POST", "/api/v1/security/keys", {"name": "validate-diff", "private_key": KEY})
    if code != 201:
        raise SystemExit(f"key: {code} {body}")
    key_uuid = body["uuid"]
    failures = 0
    created = []
    try:
        for user, alias in USERS:
            if alias != user:
                # The alias is taken by another server; give this one its own name for the host.
                alias = f"{alias}-{user}"
            code, body = api(REF, "POST", "/api/v1/servers",
                             {"name": f"vd-{user}", "ip": f"th-{alias}", "user": user, "port": 22, "private_key_uuid": key_uuid})
            if code != 201:
                raise SystemExit(f"server {user}: {code} {body}")
            uuid = body["uuid"]
            created.append(uuid)

            # OpenSSH names the host in some errors; the two sides reach it by different names.
            ref = [c.replace(f"@th-{alias}", "@HOST") for c in validate(REF, uuid)]
            reset(uuid)
            sql(f"UPDATE servers SET ip = '{RS_HOST}', port = {RS_PORT} WHERE uuid = '{uuid}'")
            rs = [c.replace(f"@{RS_HOST}", "@HOST") for c in validate(RS, uuid)]
            sql(f"UPDATE servers SET ip = 'th-{alias}', port = 22 WHERE uuid = '{uuid}'")

            same = ref == rs
            failures += not same
            print(f"{'PASS' if same else 'FAIL'} {user}")
            if not same:
                print(f"  ref: {ref}\n  rs:  {rs}")
    finally:
        for uuid in created:
            api(REF, "DELETE", f"/api/v1/servers/{uuid}")
        api(REF, "DELETE", f"/api/v1/security/keys/{key_uuid}")
    print(f"{len(USERS) - failures}/{len(USERS)} scenarios identical")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
