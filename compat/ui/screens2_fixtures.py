#!/usr/bin/env python3
"""Fixtures for the screens2 comparisons: one project with each kind of resource, created through the
reference's API and tracked by uuid.

    screens2_fixtures.py create <state.json>
    screens2_fixtures.py clean <state.json>

Nothing is deployed. Git sources name a host in `.invalid`, which nothing resolves, so no side contacts a git host.
"""
import base64
import json
import os
import sys
import urllib.error
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
TOKENS = json.load(open(os.path.join(ROOT, "compat/reference/secrets/tokens.json")))
REF = "http://127.0.0.1:18000"
SERVER = "hml20fsj5eles0xfiqidq8j5"
GIT = "https://git.example.invalid/nobody/nothing"
COMPOSE = """services:
  web:
    image: nginx:alpine
    environment:
      - SERVICE_URL_WEB_80
  db:
    image: postgres:16-alpine
    environment:
      - POSTGRES_PASSWORD=$SERVICE_PASSWORD_DB
    volumes:
      - db-data:/var/lib/postgresql/data
volumes:
  db-data:
"""


def call(method, path, body=None):
    req = urllib.request.Request(REF + path, method=method, data=json.dumps(body).encode() if body is not None else None)
    req.add_header("Authorization", "Bearer " + TOKENS["acme_owner_root"])
    req.add_header("Content-Type", "application/json")
    req.add_header("Accept", "application/json")
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        raw = e.read()
        try:
            return e.code, json.loads(raw)
        except ValueError:
            return e.code, raw[:300].decode(errors="replace")


def b64(s):
    return base64.b64encode(s.encode()).decode()


def create(path):
    state = {"apps": {}, "services": [], "databases": []}
    c, r = call("POST", "/api/v1/projects", {"name": "screens2 fixtures"})
    assert c == 201, (c, r)
    state["project"] = r["uuid"]
    save(path, state)
    c, r = call("GET", f"/api/v1/projects/{state['project']}")
    env = r["environments"][0]
    state["env"] = env["uuid"]
    common = {"project_uuid": state["project"], "server_uuid": SERVER, "environment_uuid": env["uuid"], "instant_deploy": False}
    bodies = {
        "dockerfile": ("/api/v1/applications/dockerfile", dict(common, name="s2-dockerfile", dockerfile=b64("FROM nginx:alpine\n"), ports_exposes="80")),
        "image": ("/api/v1/applications/dockerimage", dict(common, name="s2-image", docker_registry_image_name="nginx", ports_exposes="80")),
        "git": ("/api/v1/applications/public", dict(common, name="s2-git", git_repository=GIT, git_branch="main", build_pack="nixpacks", ports_exposes="3000")),
        "compose": ("/api/v1/applications/public", dict(common, name="s2-compose", git_repository=GIT, git_branch="main", build_pack="dockercompose", ports_exposes="3000")),
    }
    for key, (url, body) in bodies.items():
        c, r = call("POST", url, body)
        assert c == 201, (key, c, r)
        state["apps"][key] = r["uuid"]
        save(path, state)
    c, r = call("POST", "/api/v1/services", dict(common, name="s2-service", docker_compose_raw=b64(COMPOSE)))
    assert c == 201, (c, r)
    state["services"].append(r["uuid"])
    save(path, state)
    c, r = call("GET", f"/api/v1/services/{r['uuid']}")
    state["stack"] = [x["uuid"] for x in r["applications"] + r["databases"]]
    save(path, state)
    c, r = call("POST", "/api/v1/databases/postgresql", dict(common, name="s2-postgres"))
    assert c == 200 or c == 201, (c, r)
    state["databases"].append(r["uuid"])
    save(path, state)
    print(json.dumps(state))


def clean(path):
    state = json.load(open(path))
    for u in state.get("apps", {}).values():
        print("app", u, call("DELETE", f"/api/v1/applications/{u}?delete_configurations=true&delete_volumes=true&docker_cleanup=false&delete_connected_networks=false")[0])
    for u in state.get("services", []):
        print("service", u, call("DELETE", f"/api/v1/services/{u}?delete_configurations=true&delete_volumes=true&docker_cleanup=false&delete_connected_networks=false")[0])
    for u in state.get("databases", []):
        print("database", u, call("DELETE", f"/api/v1/databases/{u}?delete_configurations=true&delete_volumes=true&docker_cleanup=false&delete_connected_networks=false")[0])
    if state.get("project"):
        print("project", state["project"], call("DELETE", f"/api/v1/projects/{state['project']}"))


def save(path, state):
    json.dump(state, open(path, "w"))


{"create": create, "clean": clean}[sys.argv[1]](sys.argv[2])
