#!/usr/bin/env python3
"""Proves a coolify-rs install deploys on its own localhost server, over real ssh inside the VM.

    deploy_check.py <machine> <tunnel port>

Everything it creates lives on that throwaway VM. The API token is made for this run and never printed.
"""
import base64
import hashlib
import json
import secrets
import subprocess
import sys
import time
import urllib.error
import urllib.request

machine, port = sys.argv[1], sys.argv[2]
BASE = f"http://127.0.0.1:{port}"
results = []
SUFFIX = secrets.token_hex(3)


def vm(*args, check=True):
    p = subprocess.run(["orb", "-m", machine, "-u", "root", *args], capture_output=True, text=True, timeout=120)
    if check and p.returncode != 0:
        raise SystemExit(f"{args[:3]}: {p.stderr[:300]}")
    return p.stdout


def sql(statement):
    return vm("docker", "exec", "coolify-db", "psql", "-U", "coolify", "-d", "coolify", "-Atc", statement).strip()


def record(name, ok, detail=""):
    results.append(ok)
    print(("ok   " if ok else "FAIL ") + name + (f"  {detail}" if detail else ""))


plain = secrets.token_hex(20)
digest = hashlib.sha256(plain.encode()).hexdigest()
sql("UPDATE instance_settings SET is_api_enabled = true WHERE id = 0")
token_id = sql(
    "INSERT INTO personal_access_tokens (tokenable_type, tokenable_id, name, token, abilities, team_id, created_at, updated_at) "
    f"VALUES ('App\\Models\\User', 0, 'install-check', '{digest}', '[\"root\"]', '0', now(), now()) RETURNING id"
).splitlines()[0]
TOKEN = f"{token_id}|{plain}"


def api(method, path, body=None):
    req = urllib.request.Request(BASE + "/api/v1" + path, method=method, data=json.dumps(body).encode() if body is not None else None)
    req.add_header("Authorization", "Bearer " + TOKEN)
    req.add_header("Content-Type", "application/json")
    req.add_header("Accept", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        text = e.read()[:400].decode(errors="replace")
        return e.code, text


status, servers = api("GET", "/servers")
record("server list", status == 200, str(status))
server = servers[0]
status, validated = api("POST", f"/servers/{server['uuid']}/validate", {})
record("localhost validates", status in (200, 201), str(validated)[:120])
time.sleep(5)
status, project = api("POST", "/projects", {"name": "install check"})
record("project created", status == 201, str(status))
project_uuid = project["uuid"]


def wait_deployed(app_uuid, name, host):
    deadline = time.time() + 420
    state = ""
    while time.time() < deadline:
        _, app = api("GET", f"/applications/{app_uuid}")
        state = app.get("status", "") if isinstance(app, dict) else str(app)
        containers = vm("docker", "ps", "--filter", f"name={app_uuid}", "--format", "{{.Names}} {{.Status}}")
        if state.startswith("running") and containers:
            break
        time.sleep(8)
    deployment = sql(
        "SELECT q.status FROM application_deployment_queues q JOIN applications a ON a.id::text = q.application_id "
        f"WHERE a.uuid = '{app_uuid}' ORDER BY q.id DESC LIMIT 1"
    )
    record(f"{name}: deployment finished", deployment == "finished", deployment)
    record(f"{name}: application running", state.startswith("running"), state)
    answer = ""
    for _ in range(15):
        answer = vm("curl", "-s", "-m", "5", "-H", f"Host: {host}", "http://127.0.0.1/", check=False)
        if "install-check" in answer or "nginx" in answer:
            break
        time.sleep(4)
    record(f"{name}: answers through the proxy", "install-check" in answer or "nginx" in answer, answer[:60].replace("\n", " "))


status, app = api("POST", "/applications/dockerimage", {
    "project_uuid": project_uuid, "server_uuid": server["uuid"], "environment_name": "production",
    "docker_registry_image_name": "nginx", "docker_registry_image_tag": "1.27-alpine", "ports_exposes": "80",
    "name": "image-app", "domains": f"http://image-{SUFFIX}.install.test", "instant_deploy": True,
})
record("dockerimage application created", status == 201, str(app)[:120])
if status == 201:
    wait_deployed(app["uuid"], "dockerimage", f"image-{SUFFIX}.install.test")

dockerfile = "FROM nginx:1.27-alpine\nRUN echo install-check > /usr/share/nginx/html/index.html\n"
status, app = api("POST", "/applications/dockerfile", {
    "project_uuid": project_uuid, "server_uuid": server["uuid"], "environment_name": "production",
    "dockerfile": base64.b64encode(dockerfile.encode()).decode(), "ports_exposes": "80",
    "name": "built-app", "domains": f"http://built-{SUFFIX}.install.test", "instant_deploy": True,
})
record("dockerfile application created", status == 201, str(app)[:120])
if status == 201:
    wait_deployed(app["uuid"], "dockerfile (built by the helper)", f"built-{SUFFIX}.install.test")

# Remote execution is real ssh inside the VM: sshd saw the key, and the container has no other way in.
accepted = vm("bash", "-c", "journalctl -u ssh --no-pager 2>/dev/null | grep -c 'Accepted publickey for root' || true").strip()
record("sshd accepted the install's key", accepted.isdigit() and int(accepted) > 0, f"{accepted} logins")
mounts = vm("docker", "inspect", "coolify", "--format", "{{range .Mounts}}{{.Source}} {{end}}")
record("no docker socket in the coolify container", "docker.sock" not in mounts, mounts.strip())
binary = vm("docker", "exec", "coolify", "sh", "-c", "printenv COOLIFY_RS_SSH_BINARY || echo system-ssh").strip()
record("the system ssh client", binary == "system-ssh", binary)

print(f"{sum(results)}/{len(results)} passed")
sys.exit(0 if all(results) else 1)
