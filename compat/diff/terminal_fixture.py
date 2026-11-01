#!/usr/bin/env python3
"""Resources for compat/ui/terminal_ui.mjs: one running container of each kind on the test VM.

    COOLIFY_RS_BASE=http://127.0.0.1:18086 compat/diff/terminal_fixture.py up     writes the uuids to compat/ui/shots/terminal-fixture.json
    COOLIFY_RS_BASE=http://127.0.0.1:18086 compat/diff/terminal_fixture.py down   deletes exactly those uuids
    COOLIFY_RS_BASE=... compat/diff/terminal_fixture.py real-up     the VM as a second server reached with real ssh
    COOLIFY_RS_BASE=... compat/diff/terminal_fixture.py real-down

`real-up` is for a Mac without a route to the VM: a container on the Docker network forwards
127.0.0.1:2224 to the VM's sshd. Run the instance with COOLIFY_RS_SSH_BINARY=ssh, then
`node compat/ui/terminal_ui.mjs --real-ssh`.

The uuids are recorded before each deploy starts, so `down` also works after a crash.
"""
import base64
import json
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(__file__))
from databases_diff import call as raw_call, sql  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
FILE = os.path.join(ROOT, "compat/ui/shots/terminal-fixture.json")
SERVER = "hml20fsj5eles0xfiqidq8j5"
VM = "coolify-rs-e2e"
FORWARD = "terminal-stream-ssh-forward"
COMPOSE = """services:
  web:
    image: nginx:alpine
  sidecar:
    image: redis:7-alpine
    stdin_open: true
    tty: true
"""


def call(side, method, path, body=None):
    status, data = raw_call(side, method, "/api/v1" + path, body, tok="acme_owner_root")
    try:
        return status, json.loads(data)
    except ValueError:
        return status, data


def vm(script):
    return subprocess.run(["orb", "-m", VM, "-u", "root", "bash", "-c", script], capture_output=True, text=True, timeout=120).stdout


def save(state):
    os.makedirs(os.path.dirname(FILE), exist_ok=True)
    with open(FILE, "w") as f:
        json.dump(state, f, indent=1)


def must(result, what):
    status, body = result
    if status >= 300:
        raise SystemExit(f"{what}: {status} {body}")
    return body


def up():
    state = {"server": SERVER}
    project = must(call("rs", "POST", "/projects", {"name": "terminal stream"}), "project")
    state["project"] = project["uuid"]
    save(state)
    env = "production"
    environment = must(call("rs", "GET", f"/projects/{state['project']}/{env}"), "environment")
    state["environment"] = environment["uuid"]
    common = {"project_uuid": state["project"], "environment_name": env, "server_uuid": SERVER}

    db = must(call("rs", "POST", "/databases/redis", {**common, "instant_deploy": False}), "database")
    state["database"] = db["uuid"]
    save(state)
    must(call("rs", "POST", f"/databases/{state['database']}/start"), "start database")

    app = must(call("rs", "POST", "/applications/dockerimage", {
        **common,
        "docker_registry_image_name": "nginx",
        "docker_registry_image_tag": "alpine",
        "ports_exposes": "80",
        "name": "terminal-app",
        "instant_deploy": False,
    }), "application")
    state["application"] = app["uuid"]
    save(state)
    must(call("rs", "POST", f"/applications/{state['application']}/start"), "start application")

    service = must(call("rs", "POST", "/services", {
        **common,
        "name": "terminal-service",
        "docker_compose_raw": base64.b64encode(COMPOSE.encode()).decode(),
        "instant_deploy": False,
    }), "service")
    state["service"] = service["uuid"]
    save(state)
    must(call("rs", "POST", f"/services/{state['service']}/start"), "start service")

    deadline = time.time() + 240
    while time.time() < deadline:
        names = vm("docker ps --format '{{.Names}}'").split()
        have = {
            "database": state["database"] in names,
            "application": any(n.startswith(state["application"]) for n in names),
            "service": sum(n.endswith("-" + state["service"]) for n in names) == 2,
        }
        if all(have.values()):
            break
        time.sleep(5)
    print(json.dumps(have))
    print(FILE)


def real_up():
    state = json.load(open(FILE))
    key = sql(f"select k.uuid from private_keys k join servers v on v.private_key_id = k.id where v.uuid = '{SERVER}'")
    subprocess.run(["docker", "run", "-d", "--name", FORWARD, "-p", "127.0.0.1:2224:2224", "alpine/socat",
                    "TCP-LISTEN:2224,fork,reuseaddr", f"TCP:{VM}.orb.local:22"], check=True, capture_output=True)
    state["forward"] = FORWARD
    save(state)
    server = must(call("rs", "POST", "/servers", {"name": "terminal-real-ssh", "ip": "127.0.0.1", "port": 2224, "user": "root", "private_key_uuid": key}), "server")
    state["real_server"] = server["uuid"]
    save(state)
    sql("update server_settings set is_reachable = true, is_usable = true, is_terminal_enabled = true "
        f"where server_id = (select id from servers where uuid = '{state['real_server']}')")
    print(state["real_server"])


def real_ready():
    """The reference schedules over the same rows and cannot reach 127.0.0.1:2224, so it marks this row unreachable."""
    state = json.load(open(FILE))
    where = f"server_id = (select id from servers where uuid = '{state['real_server']}')"
    print(sql(f"select is_reachable, is_usable, is_terminal_enabled from server_settings where {where}"))
    sql(f"update server_settings set is_reachable = true, is_usable = true, is_terminal_enabled = true where {where}")


def real_down():
    state = json.load(open(FILE))
    if "real_server" in state:
        print("server", call("rs", "DELETE", f"/servers/{state.pop('real_server')}")[0])
    if state.pop("forward", None):
        subprocess.run(["docker", "rm", "-f", FORWARD], capture_output=True)
    save(state)


def down():
    state = json.load(open(FILE))
    if "application" in state:
        print("application", call("rs", "DELETE", f"/applications/{state['application']}?delete_volumes=true&delete_connected_networks=true&delete_configurations=true&docker_cleanup=true")[0])
    if "service" in state:
        print("service", call("rs", "DELETE", f"/services/{state['service']}?delete_volumes=true&delete_connected_networks=true&delete_configurations=true&docker_cleanup=true")[0])
    if "database" in state:
        print("database", call("rs", "DELETE", f"/databases/{state['database']}?delete_volumes=true&delete_connected_networks=true&delete_configurations=true&docker_cleanup=true")[0])
    time.sleep(20)
    if "project" in state:
        print("project", call("rs", "DELETE", f"/projects/{state['project']}")[0])
    names = vm("docker ps -a --format '{{.Names}}'").split()
    left = [n for n in names if any(state.get(k, "\0") in n for k in ("application", "service", "database"))]
    print("containers left:", left)
    if not left:
        os.remove(FILE)


if __name__ == "__main__":
    {"up": up, "down": down, "real-up": real_up, "real-ready": real_ready, "real-down": real_down}[sys.argv[1]]()
