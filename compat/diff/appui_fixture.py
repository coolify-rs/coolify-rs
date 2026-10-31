#!/usr/bin/env python3
"""Fixtures of the application and service screens (previews, scheduled tasks, resource details).

  appui_fixture.py up      creates one resource of each kind, writes the state file, prints it
  appui_fixture.py down    deletes exactly what the state file lists

Everything goes through the reference's API; ids come from the create responses. Rows the UI flows
create are deleted by those flows, and `down` deletes the resources with their tasks and previews.
"""
import base64
import json
import os
import sys
import tempfile

sys.path.insert(0, os.path.dirname(__file__))
from databases_diff import call, sql  # noqa: E402

STATE = os.path.join(tempfile.gettempdir(), "coolify-rs-appui-fixture.json")
SERVER = "hml20fsj5eles0xfiqidq8j5"
PROJECT = "j8xm2emlllhfnyebjzgyzp7z"
ENVIRONMENT = "uj3st9yvtvzxh7rul3c3opcz"
TOK = "acme_owner_root"
COMPOSE = """services:
  web:
    image: nginx:alpine
  worker:
    image: busybox:1.36
    command: sleep 3600
  db:
    image: postgres:16-alpine
    environment:
      POSTGRES_PASSWORD: appui
"""


def create(path, body):
    status, raw = call("ref", "POST", path, body, tok=TOK)
    if status not in (200, 201):
        raise SystemExit(f"{path}: {status} {raw[:300]!r}")
    return json.loads(raw)["uuid"]


def up():
    base = {
        "project_uuid": PROJECT,
        "environment_uuid": ENVIRONMENT,
        "server_uuid": SERVER,
        "instant_deploy": False,
    }
    state = json.load(open(STATE)) if os.path.exists(STATE) else {"project": PROJECT, "environment": ENVIRONMENT}
    raw = base64.b64encode(COMPOSE.encode()).decode()

    def make(key, path, body):
        # Saved after each create, so that `down` finds it when the next create fails.
        if key not in state:
            state[key] = create(path, body)
            json.dump(state, open(STATE, "w"))

    make("image", "/api/v1/applications/dockerimage", {**base, "name": "appui-image", "docker_registry_image_name": "nginx", "docker_registry_image_tag": "alpine", "ports_exposes": "80"})
    make("git", "/api/v1/applications/public", {**base, "name": "appui-git", "git_repository": "https://github.com/coollabsio/coolify-examples", "git_branch": "v4.x", "build_pack": "nixpacks", "ports_exposes": "3000"})
    make("compose", "/api/v1/applications/public", {**base, "name": "appui-compose", "git_repository": "https://github.com/coollabsio/coolify-examples", "git_branch": "v4.x", "build_pack": "dockercompose", "ports_exposes": "80", "docker_compose_location": "/docker-compose.yaml"})
    # The API has no field for this; the row is ours. The repository is a closed local port, because
    # a Compose application reads its file from there on many saves: nothing may reach GitHub.
    sql(f"UPDATE applications SET git_repository = 'http://127.0.0.1:9/appui.git', source_id = NULL, source_type = NULL WHERE uuid = '{state['compose']}'")
    sql(f"UPDATE applications SET docker_compose_raw = $raw${COMPOSE}$raw$ WHERE uuid = '{state['compose']}'")
    make("service", "/api/v1/services", {**base, "name": "appui-service", "docker_compose_raw": raw})
    print(json.dumps(state))


def down():
    state = json.load(open(STATE))
    for key, path in (("image", "applications"), ("git", "applications"), ("compose", "applications"), ("service", "services")):
        uuid = state.get(key)
        if not uuid:
            continue
        status, _ = call("ref", "DELETE", f"/api/v1/{path}/{uuid}", tok=TOK)
        print(key, uuid, status)
    print("left:", sql("SELECT count(*) FROM applications WHERE uuid IN ('%s','%s','%s') AND deleted_at IS NULL" % (state["image"], state["git"], state["compose"])))


if __name__ == "__main__":
    {"up": up, "down": down}[sys.argv[1]]()
