#!/usr/bin/env python3
"""End-to-end delivery of notifications, reference against coolify-rs.

Starts a mailpit container and a small listener (acting as Discord, Slack, Telegram, Pushover,
Resend and webhook endpoints) on the reference's network, creates a team and a user, points the
team's channels at them, delivers every case through the reference (tinker, sync queue) and through
coolify-rs (examples/notifications_send.rs), and compares what arrived: path, content type and
user agent, body bytes (`<t:...:R>` masked) and mail from, to, subject and HTML.

On the reference, Telegram and Pushover have fixed hosts, so Http::fake forwards the bodies to the
listener and their headers are not compared. Nothing goes to an external service.

Everything created is removed at the end, and the instance_settings columns it changes are saved
first and restored exactly.
"""

import base64
import json
import os
import re
import shlex
import subprocess
import sys
import tempfile
import time
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from notifications_diff import COMMITS, MAILS, NOTIFICATIONS  # noqa: E402,F401

CONTAINER = "coolify-rs-reference-coolify-1"
PG = "coolify-rs-reference-postgres-1"
NETWORK = "coolify-rs-reference_default"
MAILPIT = "coolify-rs-notif-mailpit"
LISTENER = "coolify-rs-notif-listener"
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
HERE = os.path.dirname(os.path.abspath(__file__))
LISTENER_PORT, SMTP_PORT, MAILPIT_API = 18987, 18988, 18989
RS_LISTENER = f"http://127.0.0.1:{LISTENER_PORT}"
REF_LISTENER = f"http://{LISTENER}:8080"
EMAIL = "notif-e2e@notif.test"
SAVED_COLUMNS = [
    "smtp_enabled", "smtp_from_address", "smtp_from_name", "smtp_recipients", "smtp_host", "smtp_port",
    "smtp_encryption", "smtp_username", "smtp_password", "smtp_timeout", "smtp_ehlo_domain", "resend_enabled",
    "resend_api_key", "webhook_allow_localhost", "webhook_allowed_internal_hosts", "updated_at",
]


def sh(*args, check=True, env=None, cwd=None):
    p = subprocess.run(args, capture_output=True, text=True, env=env, cwd=cwd)
    if check and p.returncode != 0:
        raise SystemExit(f"{args[:5]} failed: {p.stdout[-1500:]} {p.stderr[-1500:]}")
    return p.stdout


def psql(sql):
    return sh("docker", "exec", PG, "psql", "-U", "coolify", "-d", "coolify", "-At", "-c", sql)


def php(arg):
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "a.json")
        with open(path, "w") as f:
            json.dump(arg, f)
        sh("docker", "cp", path, f"{CONTAINER}:/tmp/notif-e2e.json")
    sh("docker", "exec", CONTAINER, "php", "/var/www/html/artisan", "tinker", "--execute", 'require "/tmp/notif-e2e-run.php";')
    out = sh("docker", "exec", CONTAINER, "cat", "/tmp/notif-e2e-out.json")
    return json.loads(out)


def http(method, url, body=None):
    req = urllib.request.Request(url, method=method, data=body)
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.loads(r.read() or b"null")


def reset():
    http("POST", f"{RS_LISTENER}/__reset", b"")
    req = urllib.request.Request(f"http://127.0.0.1:{MAILPIT_API}/api/v1/messages", method="DELETE")
    urllib.request.urlopen(req, timeout=10).read()


MASK = re.compile(rb"\\u003Ct:\d+:R\\u003E|<t:\d+:R>")


def collect():
    """What arrived, per case, sorted by path; and the mails in order."""
    cases, name = {}, "__before"
    for e in http("GET", f"{RS_LISTENER}/"):
        path = e["path"]
        if path.startswith("/__case/"):
            name = path[len("/__case/"):]
            cases.setdefault(name, [])
            continue
        body = MASK.sub(b"<t:0:R>", base64.b64decode(e["body"])).decode("utf-8", "replace")
        h = e["headers"]
        external = path.startswith(("/tg/", "/po/"))
        headers = {} if external else {k: h.get(k) for k in ("content-type", "user-agent", "authorization")}
        if path.endswith("/resend/emails") or "/resend/" in path:
            headers["user-agent"] = "(resend)"
        cases.setdefault(name, []).append({"path": path, "headers": headers, "body": body})
    for v in cases.values():
        v.sort(key=lambda x: (x["path"], x["body"]))
    mails = []
    listed = http("GET", f"http://127.0.0.1:{MAILPIT_API}/api/v1/messages?limit=500")
    for m in reversed(listed.get("messages") or []):
        full = http("GET", f"http://127.0.0.1:{MAILPIT_API}/api/v1/message/{m['ID']}")
        mails.append({
            "from": full["From"],
            "to": full["To"],
            "cc": full.get("Cc"),
            "subject": full["Subject"],
            "html": full["HTML"],
            "text": full["Text"],
        })
    return cases, mails


def rust_send(team_id, cases, extra_env=None):
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "cases.json")
        with open(path, "w") as f:
            json.dump({"listener": RS_LISTENER, "team_id": team_id, "cases": cases}, f)
        env = dict(os.environ)
        env["APP_KEY"] = sh("docker", "exec", CONTAINER, "printenv", "APP_KEY").strip()
        user = sh("docker", "exec", PG, "printenv", "POSTGRES_USER").strip()
        password = sh("docker", "exec", PG, "printenv", "POSTGRES_PASSWORD").strip()
        db = sh("docker", "exec", PG, "printenv", "POSTGRES_DB").strip()
        env["DATABASE_URL"] = f"postgres://{user}:{password}@127.0.0.1:15432/{db}"
        env["APP_URL"] = CTX["app_url"]
        env["COOLIFY_VERSION"] = CTX["version"]
        env["COOLIFY_RS_TELEGRAM_API"] = f"{RS_LISTENER}/tg"
        env["COOLIFY_RS_PUSHOVER_API"] = f"{RS_LISTENER}/po"
        env["COOLIFY_RS_RESEND_API"] = f"{RS_LISTENER}/resend"
        env.update(extra_env or {})
        out = sh("cargo", "run", "-q", "-p", "coolify-jobs", "--example", "notifications_send", "--", path, env=env, cwd=ROOT)
    return json.loads(out)


CTX = {}
RESULTS = {"compared": 0, "same": 0, "failures": []}


def compare(label, ref, rs):
    RESULTS["compared"] += 1
    if ref == rs:
        RESULTS["same"] += 1
        return
    RESULTS["failures"].append(label)
    print(f"--- {label}")
    for path, a, b in diff(ref, rs, "$"):
        print(f"  {path}\n    ref: {a}\n     rs: {b}")


def diff(a, b, path):
    """The leaves that differ, cut around the first differing character."""
    if isinstance(a, dict) and isinstance(b, dict):
        for k in sorted(set(a) | set(b)):
            yield from diff(a.get(k), b.get(k), f"{path}.{k}")
    elif isinstance(a, list) and isinstance(b, list) and len(a) == len(b):
        for i, (x, y) in enumerate(zip(a, b)):
            yield from diff(x, y, f"{path}[{i}]")
    elif a != b:
        if isinstance(a, str) and isinstance(b, str):
            i = next((n for n, (x, y) in enumerate(zip(a, b)) if x != y), min(len(a), len(b)))
            start = max(0, i - 80)
            yield path, repr(a[start:i + 200]), repr(b[start:i + 200])
        else:
            yield path, json.dumps(a, ensure_ascii=False)[:600], json.dumps(b, ensure_ascii=False)[:600]


def round_trip(label, team_id, cases, php_extra=None, rs_cases=None, configure=None):
    """The same cases on both sides. `configure(side)` prepares the settings for that side."""
    configure("ref")
    reset()
    ref_status = php({"cmd": "send", "team_id": team_id, "listener": REF_LISTENER, "cases": cases, **(php_extra or {})})
    time.sleep(0.5)
    ref_http, ref_mail = collect()
    configure("rs")
    reset()
    rs_status = rust_send(team_id, rs_cases or cases)
    rs_http, rs_mail = collect()
    for case in cases:
        name = case["name"]
        compare(f"{label}/{name}/http", ref_http.get(name), rs_http.get(name))
        compare(f"{label}/{name}/status", ref_status.get(name), rs_status.get(name))
    compare(f"{label}/mails", ref_mail, rs_mail)
    return ref_http, rs_http, ref_mail, rs_mail


def configure_team(team_id, email=None, discord=None):
    def run(side):
        arg = {
            "cmd": "configure",
            "team_id": team_id,
            "listener": REF_LISTENER if side == "ref" else RS_LISTENER,
            "smtp_host": MAILPIT if side == "ref" else "127.0.0.1",
            "smtp_port": 1025 if side == "ref" else SMTP_PORT,
            "email": email or {},
        }
        if discord:
            arg["discord_url"] = (REF_LISTENER if side == "ref" else RS_LISTENER) + discord
        php(arg)
    return run


def main():
    only = sys.argv[1] if len(sys.argv) > 1 else None
    # The root team must not have a real channel enabled: internal notifications go there.
    for ch in ("discord", "slack", "telegram", "pushover", "webhook"):
        if psql(f"select {ch}_enabled from {ch}_notification_settings where team_id = 0").strip() == "t":
            raise SystemExit(f"root team has {ch} enabled, refusing to run")

    sh("docker", "rm", "-f", MAILPIT, LISTENER, check=False)
    sh("docker", "run", "-d", "--name", MAILPIT, "--network", NETWORK, "-p", f"127.0.0.1:{SMTP_PORT}:1025", "-p", f"127.0.0.1:{MAILPIT_API}:8025", "axllent/mailpit:latest", "--smtp-auth-accept-any", "--smtp-auth-allow-insecure")
    sh("docker", "run", "-d", "--name", LISTENER, "--network", NETWORK, "-p", f"127.0.0.1:{LISTENER_PORT}:8080", "-v", f"{HERE}/notifications_listener.py:/app/listener.py:ro", "python:3.12-alpine", "python", "-u", "/app/listener.py")
    time.sleep(2)

    sh("docker", "cp", os.path.join(HERE, "notifications_e2e.php"), f"{CONTAINER}:/tmp/notif-e2e-run.php")
    sh("docker", "cp", os.path.join(HERE, "notifications_ref.php"), f"{CONTAINER}:/tmp/notif-ref.php")
    # The instance settings are shared: save the columns this test changes.
    saved = psql("select json_build_object(" + ",".join(f"'{c}',{c}" for c in SAVED_COLUMNS) + ")::text from instance_settings where id = 0")
    ids = None
    try:
        ctx_out = sh("docker", "exec", CONTAINER, "php", "/var/www/html/artisan", "tinker", "--execute", 'echo json_encode(["app_url"=>config("app.url"),"version"=>config("constants.coolify.version")]);')
        CTX.update(json.loads(ctx_out.strip().splitlines()[-1]))
        ids = php({"cmd": "setup", "email": EMAIL})
        team_id = ids["team_id"]
        allow = {
            "webhook_allow_localhost": True,
            "webhook_allowed_internal_hosts": [LISTENER, "127.0.0.1"],
        }
        php({"cmd": "instance", "values": allow})

        cases = [{"name": k, "notification": v} for k, v in NOTIFICATIONS.items()]
        if only != "fast":
            # 1. Every notification, every channel, SMTP with the team's settings.
            round_trip("smtp", team_id, cases, configure=configure_team(team_id, email={"smtp_enabled": True}))

        # 2. Resend with the team's settings.
        resend_email = {"resend_enabled": True, "resend_api_key": "re_test_key"}
        some = [c for c in cases if c["name"] in ("deploy-success", "backup-failed-multi", "unreachable")]
        round_trip("resend", team_id, some, php_extra={"resend_base": f"{REF_LISTENER}/resend"}, configure=configure_team(team_id, email=resend_email))

        # 3. The instance's email: use_instance_email_settings, and transactional mail.
        php({"cmd": "instance", "values": {**allow, "smtp_enabled": True, "smtp_from_address": "instance@notif.test", "smtp_from_name": "Instance", "smtp_host": MAILPIT, "smtp_port": 1025, "smtp_encryption": "none", "smtp_timeout": 10}})
        instance_rs = {"smtp_host": "127.0.0.1", "smtp_port": SMTP_PORT}
        instance_ref = {"smtp_host": MAILPIT, "smtp_port": 1025}

        def configure_instance(side):
            configure_team(team_id, email={"use_instance_email_settings": True})(side)
            php({"cmd": "instance", "values": instance_ref if side == "ref" else instance_rs})

        mails = [{"name": k, "mail": v, "to": EMAIL} for k, v in MAILS.items()]
        mails[3]["to"] = v_to = MAILS["mail-email-change"]["new_email"]
        assert v_to
        round_trip("instance", team_id, some, configure=configure_instance)
        round_trip("transactional", team_id, mails, configure=configure_instance)

        # 4. Delivery failures: three 404s disable Discord, write the audit row, mail the admins
        # and tell the other channels.
        def configure_failing(side):
            configure_team(team_id, email={"smtp_enabled": True}, discord="/status/404/discord")(side)
            php({"cmd": "instance", "values": instance_ref if side == "ref" else instance_rs})

        general = {"type": "General", "message": "m", "success": True}
        failing = [
            {"name": f"fail-{i}", "notification": general, "settle": True} for i in range(1, 4)
        ]
        php({"cmd": "clear_audit", "team_id": team_id})
        round_trip("failure", team_id, failing, configure=configure_failing)
        state_rs = php({"cmd": "state", "team_id": team_id})
        state_rs["audit"] = state_rs["audit"][-1:]
        php({"cmd": "clear_audit", "team_id": team_id})
        configure_failing("ref")
        reset()
        php({"cmd": "send", "team_id": team_id, "listener": REF_LISTENER, "cases": failing})
        state_ref = php({"cmd": "state", "team_id": team_id})
        for s in (state_rs, state_ref):
            for a in s["audit"]:
                a["metadata"] = json.loads(a["metadata"]) if isinstance(a["metadata"], str) else a["metadata"]
        compare("failure/state", state_ref, state_rs)
        print(json.dumps(state_rs, ensure_ascii=False)[:600])
        php({"cmd": "clear_audit", "team_id": team_id})

        # 5. notify_test (rs only: the reference page has the same request as round 1's Test case).
        configure_team(team_id, email={"smtp_enabled": True})("rs")
        reset()
        tests = [
            {"name": f"t-{ch}", "test": {"channel": ch, "emails": EMAIL if ch == "email" else None}}
            for ch in ("discord", "slack", "telegram", "pushover", "webhook", "email")
        ]
        status = rust_send(team_id, tests)
        rs_http, rs_mail = collect()
        print("notify_test:", status, {k: [e["path"] for e in v] for k, v in rs_http.items()}, [m["subject"] for m in rs_mail])
        compare("notify_test/status", {t["name"]: "ok" for t in tests}, status)
    finally:
        if saved.strip():
            values = json.loads(saved)
            assigns = ", ".join(f"{c} = (j->>'{c}')::{t}" for c, t in column_types().items())
            quoted = json.dumps(values).replace("'", "''")
            psql(f"update instance_settings set {assigns} from (select '{quoted}'::json as j) x where id = 0")
            after = psql("select json_build_object(" + ",".join(f"'{c}',{c}" for c in SAVED_COLUMNS) + ")::text from instance_settings where id = 0")
            print("instance settings restored:", json.loads(after) == values)
        if ids:
            sh("docker", "cp", os.path.join(HERE, "notifications_e2e.php"), f"{CONTAINER}:/tmp/notif-e2e-run.php")
            php({"cmd": "cleanup", **ids})
            left = psql(f"select count(*) from teams where id = {ids['team_id']}").strip() + "," + psql(f"select count(*) from users where id = {ids['user_id']}").strip()
            print("left behind (team, user):", left)
        sh("docker", "exec", "-u", "root", CONTAINER, "rm", "-f", "/tmp/notif-e2e.json", "/tmp/notif-e2e-out.json", "/tmp/notif-e2e-run.php", "/tmp/notif-ref.php", check=False)
        sh("docker", "rm", "-f", MAILPIT, LISTENER, check=False)
    print(f"{RESULTS['compared']} comparisons, {RESULTS['same']} same, {len(RESULTS['failures'])} different")
    if RESULTS["failures"]:
        print("different:", shlex.join(RESULTS["failures"]))
        sys.exit(1)


def column_types():
    rows = psql("select column_name, udt_name from information_schema.columns where table_name = 'instance_settings'")
    types = dict(line.split("|") for line in rows.splitlines() if line)
    return {c: types[c] for c in SAVED_COLUMNS}


if __name__ == "__main__":
    main()
