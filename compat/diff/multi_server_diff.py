#!/usr/bin/env python3
"""Additional servers, build servers, registry pushes and Swarm: upstream vs coolify-rs.

    COOLIFY_RS_BASE=http://127.0.0.1:18090 multi_server_diff.py --vm2 coolify-rs-agent-deploy \
        [--case registry,missing-image,multi,build-noconfig,build,swarm] [--keep-server]

Needs a second OrbStack machine with Docker and the test key (the deploy stream creates it). A local
registry runs on the test VM and answers on 127.0.0.1:5000 on both machines, so no image leaves them.
Every case uses inline Dockerfiles built from images already pulled. Each side deploys its own
application; rows, log entries, hidden commands, Compose files and containers are compared.
Everything created is tracked by uuid and deleted at the end, also after a crash in a case.
"""
import argparse
import base64
import http.cookiejar
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import databases_diff as d  # noqa: E402
from databases_diff import call as raw_call, record, results, sql  # noqa: E402


class Rs:
    """A session on the coolify-rs instance, for what has no API route."""

    def __init__(self):
        self.jar = http.cookiejar.CookieJar()
        self.open = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(self.jar))
        self.base = d.BASES["rs"]
        login = os.path.join(d.ROOT, "compat/reference/secrets/ui-login.txt")
        password = re.search(r"^password:\s*(.+)$", open(login).read(), re.M).group(1).strip()
        self.password = password
        self.csrf = ""
        _, b = self.post("/ui/session", {"email": "alice@example.test", "password": password})
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


VM1 = "coolify-rs-e2e"
SERVER1 = "hml20fsj5eles0xfiqidq8j5"
TOK = "acme_owner_root"
KEY_ID = 60
REGISTRY = "coolify-rs-deploy-registry"
FORWARD = "coolify-rs-deploy-forward"
DOCKERFILE = base64.b64encode(b"FROM nginx:alpine\nRUN echo multi > /usr/share/nginx/html/index.html\n").decode()
END = ("finished", "failed", "cancelled-by-user")
SKIP = {"id", "application_id", "deployment_uuid", "created_at", "updated_at", "finished_at", "deployment_url",
        "application_name", "horizon_job_id", "horizon_job_worker", "current_process_id", "logs",
        "configuration_snapshot", "configuration_hash", "configuration_diff", "is_api", "parent_deployment_uuid"}


def call(side, method, path, body=None):
    c, b = raw_call(side, method, "/api/v1" + path, body, TOK)
    try:
        return c, json.loads(b)
    except ValueError:
        return c, b.decode(errors="replace")


def vm(machine, cmd):
    return subprocess.run(["orb", "-m", machine, "-u", "root", "bash", "-c", cmd], capture_output=True, text=True).stdout


def rows(u):
    out = sql(f"SELECT row_to_json(q)::text FROM application_deployment_queues q WHERE application_id=(SELECT id::text FROM applications WHERE uuid='{u}') ORDER BY id")
    return [json.loads(r) for r in out.splitlines() if r.strip()]


def wait(u, count, timeout=500):
    """All of the first `count` deployments ended, the helper of each stopped."""
    for _ in range(timeout // 2):
        q = rows(u)
        # A deployment refused before it starts has no finished_at and no helper.
        if len(q) >= count and all(x["status"] == "failed" and "Before deploying to multiple servers" in (x["logs"] or "")
                                   or x["status"] in END and x["finished_at"] and "Gracefully shutting down build container" in (x["logs"] or "") for x in q[:count]):
            return q
        time.sleep(2)
    return rows(u)


def norm(text, pairs):
    for old, new in pairs:
        if old:
            text = text.replace(old, new)
    text = re.sub(r"-\d{8}[Tt]\d{6}", "-TS", text)
    text = re.sub(r"\b[a-z0-9]{24}\b", "UUID", text)
    text = re.sub(r"\b[0-9a-f]{64}\b", "SHA", text)
    text = re.sub(r"\b[0-9a-f]{12}\b", "ID", text)
    text = re.sub(r"sha256:[0-9a-f]+", "sha256:X", text)
    text = re.sub(r"http://[^ ]*/project/", "BASE/project/", text)
    text = re.sub(r"time=\"[^\"]*\"", 'time="T"', text)
    return text


def entries(q, pairs):
    visible, commands, seen = [], [], set()
    compose = []
    for e in json.loads(q["logs"] or "[]"):
        cmd = e.get("command")
        if cmd:
            c = norm(cmd, pairs)
            # Health checks repeat per attempt; timing decides how many.
            if ".State.Health." in c or "COOLIFY_BUILD_SECRETS" in c:
                c = re.sub(r"\s+", " ", c)
            if (e["batch"], c) not in seen:
                seen.add((e["batch"], c))
                commands.append(c)
        if e["hidden"]:
            continue
        t = norm(e["output"], pairs)
        if t.startswith("Attempt "):
            continue
        if cmd is None and (t.startswith("Container ") or t.startswith("time=") or t.startswith(" Container")):
            compose.extend(x.strip() for x in t.splitlines())
            continue
        visible.append((e["type"], t))
    visible.append(("compose", "\n".join(sorted(compose))))
    return visible, commands


def compare_row(label, qr, qs, pairs_r, pairs_s):
    a = {k: v for k, v in qr.items() if k not in SKIP}
    b = {k: v for k, v in qs.items() if k not in SKIP}
    record(f"{label}: rows alike", a == b, {k: (a[k], b.get(k)) for k in a if a[k] != b.get(k)})
    vr, cr = entries(qr, pairs_r)
    vs, cs = entries(qs, pairs_s)
    first = next(((x, y) for x, y in zip(vr, vs) if x != y), None)
    record(f"{label}: visible log", vr == vs, f"{len(vr)} vs {len(vs)}, first {first}")
    first = next(((x, y) for x, y in zip(cr, cs) if x != y), None)
    record(f"{label}: commands", cr == cs, f"{len(cr)} vs {len(cs)}, first {first}, tail {cr[-2:]} {cs[-2:]}")


class Env:
    def __init__(self, a):
        self.a = a
        self.vm2 = a.vm2
        self.server2 = None
        self.server2_id = None
        self.project = None
        self.apps = []
        self.services = []
        self.databases = []
        self.cleanups = []
        self.dest_rows = []
        self.swarm_dest = None

    def prepare(self):
        """The registry and the second server."""
        vm(VM1, f"docker inspect {REGISTRY} >/dev/null 2>&1 || docker run -d --name {REGISTRY} --restart=no -p 5000:5000 registry:2")
        ip = vm(VM1, "hostname -I | cut -d' ' -f1").strip()
        vm(self.vm2, f"docker inspect {FORWARD} >/dev/null 2>&1 || docker run -d --name {FORWARD} --restart=no --network host alpine/socat tcp-listen:5000,fork,reuseaddr,bind=127.0.0.1 tcp-connect:{ip}:5000")
        vm(self.vm2, "docker pull nginx:alpine >/dev/null 2>&1")
        key = sql(f"SELECT uuid FROM private_keys WHERE id={KEY_ID}")
        # A run stopped halfway can leave the server of this machine; nothing else uses that address.
        self.server2 = sql(f"SELECT uuid FROM servers WHERE ip='{self.vm2}.orb.local' AND team_id=4 AND deleted_at IS NULL")
        if not self.server2:
            c, s = call("ref", "POST", "/servers", {"name": "multi-diff-vm2", "ip": f"{self.vm2}.orb.local", "port": 22, "user": "root",
                                                    "private_key_uuid": key, "instant_validate": False, "proxy_type": "none"})
            record("second server created", c == 201, s)
            self.server2 = s["uuid"]
        self.server2_id = int(sql(f"SELECT id FROM servers WHERE uuid='{self.server2}'"))
        call("ref", "GET", f"/servers/{self.server2}/validate")
        for _ in range(60):
            if sql(f"SELECT is_usable AND is_reachable FROM server_settings WHERE server_id={self.server2_id}") == "t":
                break
            time.sleep(3)
        record("second server validated", sql(f"SELECT is_usable AND is_reachable FROM server_settings WHERE server_id={self.server2_id}") == "t")
        if not sql(f"SELECT id FROM standalone_dockers WHERE server_id={self.server2_id}"):
            uuid = f"msd{os.getpid()}destination"
            sql(f"INSERT INTO standalone_dockers (name, uuid, network, server_id, created_at, updated_at) VALUES ('multi-diff', '{uuid}', 'coolify', {self.server2_id}, now(), now())")
        vm(self.vm2, "docker network inspect coolify >/dev/null 2>&1 || docker network create --attachable coolify")
        self.dest2 = int(sql(f"SELECT id FROM standalone_dockers WHERE server_id={self.server2_id} LIMIT 1"))
        _, p = call("ref", "POST", "/projects", {"name": "multi-server-diff"})
        self.project = p["uuid"]

    def create_pair(self, case, image=True, server=SERVER1, **extra):
        out = {}
        for side in ("ref", "rs"):
            body = {"server_uuid": server, "project_uuid": self.project, "environment_name": "production", "name": f"{case}-{side}",
                    "dockerfile": DOCKERFILE, "ports_exposes": "80", "instant_deploy": False, **extra}
            c, r = call(side, "POST", "/applications/dockerfile", body)
            record(f"[{case}] {side} created", c == 201, r)
            if c != 201:
                raise RuntimeError(f"{case}: {r}")
            self.apps.append(r["uuid"])
            out[side] = r["uuid"]
            if image:
                sql(f"UPDATE applications SET docker_registry_image_name='localhost:5000/{case}-{side}', docker_registry_image_tag='v1' WHERE uuid='{r['uuid']}'")
        return out

    def attach(self, uuids):
        for u in uuids.values():
            sql(f"INSERT INTO additional_destinations (application_id, standalone_docker_id, server_id, status, created_at, updated_at) "
                f"SELECT id, {self.dest2}, {self.server2_id}, 'exited', now(), now() FROM applications WHERE uuid='{u}'")

    def cleanup(self):
        for u in self.services:
            call("ref", "DELETE", f"/services/{u}?delete_volumes=true")
        for u in self.databases:
            call("ref", "DELETE", f"/databases/{u}?delete_volumes=true")
        tables = {"services": self.services, "standalone_redis": self.databases}
        for _ in range(60):
            if all(not uuids or sql("SELECT count(*) FROM {} WHERE uuid IN ({})".format(t, ",".join(f"'{u}'" for u in uuids))) == "0" for t, uuids in tables.items()):
                break
            time.sleep(3)
        for machine, command in self.cleanups:
            vm(machine, command)
        for u in self.apps:
            call("ref", "DELETE", f"/applications/{u}?delete_volumes=true")
        for _ in range(60):
            if not self.apps or sql("SELECT count(*) FROM applications WHERE uuid IN ({})".format(",".join(f"'{u}'" for u in self.apps))) == "0":
                break
            time.sleep(3)
        for u in self.apps:
            for machine in (VM1, self.vm2):
                vm(machine, f"docker stack rm {u} >/dev/null 2>&1; docker ps -aq --filter name={u} | xargs -r docker rm -f; docker network rm {u} >/dev/null 2>&1; rm -rf /data/coolify/applications/{u}")
        if self.project:
            for _ in range(10):
                if call("ref", "DELETE", f"/projects/{self.project}")[0] in (200, 404):
                    break
                time.sleep(3)
        sql(f"UPDATE server_settings SET is_swarm_manager=false WHERE server_id={self.server2_id}") if self.server2_id else None
        if self.server2 and not self.a.keep_server:
            sql(f"UPDATE server_settings SET is_build_server=false, server_role=NULL, is_swarm_manager=false WHERE server_id={self.server2_id}")
            if self.swarm_dest:
                sql(f"DELETE FROM swarm_dockers WHERE uuid='{self.swarm_dest}'")
            sql(f"DELETE FROM additional_destinations WHERE server_id={self.server2_id}")
            c, r = call("ref", "DELETE", f"/servers/{self.server2}")
            print("second server deleted:", c, str(r)[:120])
        vm(self.vm2, "docker swarm leave --force >/dev/null 2>&1; rm -f /root/.docker/config.json")
        if not self.a.keep_server:
            vm(self.vm2, f"docker rm -f {FORWARD} >/dev/null 2>&1")
            vm(VM1, f"docker rm -f {REGISTRY} >/dev/null 2>&1")
        for name in ("registry", "multi", "build", "missing"):
            vm(VM1, f"docker images --format '{{{{.Repository}}}}:{{{{.Tag}}}}' | grep '^localhost:5000/{name}' | xargs -r docker rmi -f >/dev/null 2>&1")
            vm(self.vm2, f"docker images --format '{{{{.Repository}}}}:{{{{.Tag}}}}' | grep '^localhost:5000/{name}' | xargs -r docker rmi -f >/dev/null 2>&1")


def deploy_pair(u, n=1):
    for side in ("ref", "rs"):
        c, r = call(side, "POST", f"/deploy?uuid={u[side]}")
        record(f"deploy accepted by {side}", c == 200, r)
    return wait(u["ref"], n), wait(u["rs"], n)


def pairs(u, side, case):
    return [(u[side], "U"), (f"{case}-{side}", "NAME")]


def case_registry(e):
    u = e.create_pair("registry")
    qr, qs = deploy_pair(u)
    record("[registry] both finished", qr and qs and qr[0]["status"] == qs[0]["status"] == "finished", (qr and qr[0]["status"], qs and qs[0]["status"]))
    compare_row("[registry]", qr[0], qs[0], pairs(u, "ref", "registry"), pairs(u, "rs", "registry"))
    catalog = vm(VM1, "curl -s http://127.0.0.1:5000/v2/_catalog")
    for side in ("ref", "rs"):
        tags = vm(VM1, f"curl -s http://127.0.0.1:5000/v2/registry-{side}/tags/list")
        record(f"[registry] {side} pushed latest and v1", '"latest"' in tags and '"v1"' in tags, f"{catalog} {tags}")


def case_missing(e):
    u = e.create_pair("missing-image", image=False)
    e.attach(u)
    qr, qs = deploy_pair(u)
    record("[missing-image] both failed", qr and qs and qr[0]["status"] == qs[0]["status"] == "failed", (qr and qr[0]["status"], qs and qs[0]["status"]))
    for q, name in ((qr, "ref"), (qs, "rs")):
        record(f"[missing-image] {name} says why", "you must set a Docker image" in (q[0]["logs"] or ""), (q[0]["logs"] or "")[:300])


def containers(machine, u):
    return vm(machine, f"docker ps --format '{{{{.Names}}}}' --filter name={u}").split()


def case_multi(e, rs):
    u = e.create_pair("multi")
    e.attach(u)
    qr, qs = deploy_pair(u, 2)
    record("[multi] two deployments each", len(qr) == 2 and len(qs) == 2, (len(qr), len(qs)))
    if len(qr) != 2 or len(qs) != 2:
        return
    for i, name in ((0, "main"), (1, "additional")):
        record(f"[multi] {name}: both finished", qr[i]["status"] == qs[i]["status"] == "finished", (qr[i]["status"], qs[i]["status"]))
        compare_row(f"[multi] {name}", qr[i], qs[i], pairs(u, "ref", "multi"), pairs(u, "rs", "multi"))
    for q, name in ((qr, "ref"), (qs, "rs")):
        record(f"[multi] {name}: child points at parent", q[1]["parent_deployment_uuid"] == q[0]["deployment_uuid"] and int(q[1]["server_id"]) == e.server2_id,
               (q[1]["parent_deployment_uuid"], q[0]["deployment_uuid"], q[1]["server_id"]))
    for side in ("ref", "rs"):
        record(f"[multi] {side}: runs on both servers", bool(containers(VM1, u[side])) and bool(containers(e.vm2, u[side])), (containers(VM1, u[side]), containers(e.vm2, u[side])))
    # GET /applications with additional servers: one application through both sides.
    for u_side in ("ref", "rs"):
        c1, a1 = call("ref", "GET", f"/applications/{u[u_side]}")
        c2, a2 = call("rs", "GET", f"/applications/{u[u_side]}")
        # The schedulers touch the server rows and the status between the two requests.
        drop = ("updated_at", "last_online_at", "destination", "additional_servers")
        strip = lambda x: {k: v for k, v in x.items() if k not in drop} if isinstance(x, dict) else x
        record(f"[multi] show of the {u_side} application", c1 == c2 and strip(a1) == strip(a2) and list(a1) == list(a2),
               {k: (a1.get(k), a2.get(k)) for k in a1 if strip(a1).get(k) != strip(a2).get(k)})
        s1 = {k: v for k, v in a1["additional_servers"][0].items() if k not in ("settings", "updated_at", "sentinel_updated_at")}
        s2 = {k: v for k, v in a2["additional_servers"][0].items() if k not in ("settings", "updated_at", "sentinel_updated_at")}
        record(f"[multi] additional_servers of the {u_side} application", s1 == s2 and list(s1) == list(s2), {k: (s1.get(k), s2.get(k)) for k in set(s1) | set(s2) if s1.get(k) != s2.get(k)})
    cr, sr = vm(e.vm2, f"cat /data/coolify/applications/{u['ref']}/docker-compose.yaml"), vm(e.vm2, f"cat /data/coolify/applications/{u['rs']}/docker-compose.yaml")
    record("[multi] additional compose file", norm(cr, pairs(u, "ref", "multi")) == norm(sr, pairs(u, "rs", "multi")), "differs")
    # Statuses: the reference's scheduler writes both; the status of coolify-rs comes from the page's refresh.
    code, b = rs.post(f"/ui/resources/application/{u['rs']}/destination/stop", {"server_id": e.server2_id})
    record("[multi] rs one-server stop accepted", code == 200, b[:200])
    time.sleep(1)
    record("[multi] rs one-server stop removed the container", not containers(e.vm2, u["rs"]) and bool(containers(VM1, u["rs"])), containers(e.vm2, u["rs"]))
    pivot = sql(f"SELECT status FROM additional_destinations WHERE application_id=(SELECT id FROM applications WHERE uuid='{u['rs']}')")
    record("[multi] rs status of the additional server", pivot.startswith("exited"), pivot)
    # redeploy($network_id, $server_id): only that server.
    code, b = rs.post(f"/ui/resources/application/{u['rs']}/destination/redeploy", {"network_id": e.dest2, "server_id": e.server2_id})
    record("[multi] rs redeploy accepted", code == 200 and b"deployment_uuid" in b, b[:200])
    q = wait(u["rs"], 3)
    record("[multi] rs redeploy is one server only", len(q) == 3 and q[2]["only_this_server"] and q[2]["status"] == "finished" and int(q[2]["server_id"]) == e.server2_id,
           [(x["only_this_server"], x["status"], x["server_id"]) for x in q])
    time.sleep(3)
    record("[multi] rs redeploy queued nothing else", len(rows(u["rs"])) == 3)
    record("[multi] rs back on the additional server", bool(containers(e.vm2, u["rs"])))
    for side in ("ref", "rs"):
        c, r = call(side, "POST", f"/applications/{u[side]}/stop")
        record(f"[multi] {side} stop accepted", c == 200, r)
    for _ in range(30):
        if not any(containers(m, u[s]) for m in (VM1, e.vm2) for s in ("ref", "rs")):
            break
        time.sleep(3)
    for side in ("ref", "rs"):
        record(f"[multi] {side} stop removes both", not containers(VM1, u[side]) and not containers(e.vm2, u[side]), (containers(VM1, u[side]), containers(e.vm2, u[side])))


def case_build(e, with_config):
    case = "build" if with_config else "build-noconfig"
    sql(f"UPDATE server_settings SET server_role='build', is_build_server=true WHERE server_id={e.server2_id}")
    if with_config:
        vm(e.vm2, "mkdir -p /root/.docker; test -f /root/.docker/config.json || echo '{}' > /root/.docker/config.json")
    else:
        vm(e.vm2, "rm -f /root/.docker/config.json")
    try:
        u = e.create_pair(case)
        for side in ("ref", "rs"):
            sql(f"UPDATE application_settings SET is_build_server_enabled=true WHERE application_id=(SELECT id FROM applications WHERE uuid='{u[side]}')")
        qr, qs = deploy_pair(u)
        want = "finished" if with_config else "failed"
        record(f"[{case}] both {want}", qr and qs and qr[0]["status"] == qs[0]["status"] == want, (qr and qr[0]["status"], qs and qs[0]["status"]))
        compare_row(f"[{case}]", qr[0], qs[0], pairs(u, "ref", case), pairs(u, "rs", case))
        if with_config:
            for side in ("ref", "rs"):
                record(f"[{case}] {side}: runs on the main server", bool(containers(VM1, u[side])) and not containers(e.vm2, u[side]))
                record(f"[{case}] {side}: helper gone from the build server", qr[0]["deployment_uuid"] not in vm(e.vm2, "docker ps -a --format '{{.Names}}'"))
    finally:
        sql(f"UPDATE server_settings SET server_role=NULL, is_build_server=false WHERE server_id={e.server2_id}")


def case_swarm(e):
    vm(e.vm2, "docker swarm init >/dev/null 2>&1; docker network inspect coolify-overlay >/dev/null 2>&1 || docker network create --driver overlay --attachable coolify-overlay")
    e.swarm_dest = sql(f"SELECT uuid FROM swarm_dockers WHERE server_id={e.server2_id}")
    if not e.swarm_dest:
        e.swarm_dest = f"msd{os.getpid()}swarm"
        sql(f"INSERT INTO swarm_dockers (name, uuid, network, server_id, created_at, updated_at) VALUES ('multi-diff-swarm', '{e.swarm_dest}', 'coolify-overlay', {e.server2_id}, now(), now())")
    sql(f"UPDATE server_settings SET is_swarm_manager=true WHERE server_id={e.server2_id}")
    sql(f"DELETE FROM standalone_dockers WHERE id={e.dest2} AND NOT EXISTS (SELECT 1 FROM additional_destinations WHERE standalone_docker_id={e.dest2})")
    # A generated container name makes the service name too long for Swarm on both sides.
    u = e.create_pair("swarm-long", server=e.server2, destination_uuid=e.swarm_dest)
    qr, qs = deploy_pair(u)
    record("[swarm-long] both failed", qr and qs and qr[0]["status"] == qs[0]["status"] == "failed", (qr and qr[0]["status"], qs and qs[0]["status"]))
    compare_row("[swarm-long]", qr[0], qs[0], pairs(u, "ref", "swarm-long"), pairs(u, "rs", "swarm-long"))
    for side in ("ref", "rs"):
        record(f"[swarm-long] {side} says why", "name must be 63 characters or fewer" in (qr if side == "ref" else qs)[0]["logs"])
    u = e.create_pair("swarm", server=e.server2, destination_uuid=e.swarm_dest)
    for side in ("ref", "rs"):
        c, r = call(side, "GET", f"/applications/{u[side]}")
        record(f"[swarm] {side} destination", c == 200 and r.get("destination", {}).get("network") == "coolify-overlay", str(r)[:300])
    c1, a = call("ref", "GET", f"/applications/{u['ref']}")
    c2, b = call("rs", "GET", f"/applications/{u['ref']}")
    strip = ("updated_at",)
    record("[swarm] show alike", c1 == c2 and {k: v for k, v in a.items() if k not in strip} == {k: v for k, v in b.items() if k not in strip},
           {k: (a.get(k), b.get(k)) for k in a if a.get(k) != b.get(k)})
    for side in ("ref", "rs"):
        sql(f"UPDATE application_settings SET is_consistent_container_name_enabled=true, is_swarm_only_worker_nodes=false WHERE application_id=(SELECT id FROM applications WHERE uuid='{u[side]}')")
    qr, qs = deploy_pair(u)
    record("[swarm] both finished", qr and qs and qr[0]["status"] == qs[0]["status"] == "finished", [[e["output"][:900] for e in json.loads(q[0]["logs"] or "[]") if e["type"] == "stderr"][-4:] for q in (qr, qs) if q])
    compare_row("[swarm]", qr[0], qs[0], pairs(u, "ref", "swarm"), pairs(u, "rs", "swarm"))
    cr = vm(e.vm2, f"cat /data/coolify/applications/{u['ref']}/docker-compose.yaml")
    cs = vm(e.vm2, f"cat /data/coolify/applications/{u['rs']}/docker-compose.yaml")
    record("[swarm] compose file", norm(cr, pairs(u, "ref", "swarm")) == norm(cs, pairs(u, "rs", "swarm")), f"{cr}\n----\n{cs}")
    for side in ("ref", "rs"):
        s = vm(e.vm2, f"docker stack services {u[side]} --format '{{{{.Name}}}} {{{{.Replicas}}}}'")
        for _ in range(20):
            if "1/1" in s:
                break
            time.sleep(3)
            s = vm(e.vm2, f"docker stack services {u[side]} --format '{{{{.Name}}}} {{{{.Replicas}}}}'")
        record(f"[swarm] {side} service runs", "1/1" in s, s + vm(e.vm2, f"docker service ps {u[side]}_{u[side]} --no-trunc --format '{{{{.CurrentState}}}} {{{{.Error}}}}' | head -3"))
    # GetContainersStatus reads the replicas of the service.
    rs = Rs()
    rs.post(f"/ui/resources/application/{u['rs']}/destination/stop", {"server_id": e.server2_id})
    status = sql(f"SELECT status FROM applications WHERE uuid='{u['rs']}'")
    record("[swarm] rs status from the service replicas", status == "running:healthy", status)
    c, r = call("rs", "GET", f"/applications/{u['rs']}")
    record("[swarm] rs server_status", r.get("server_status") is True and r.get("status") == "running:healthy", (r.get("server_status"), r.get("status")))
    for side in ("ref", "rs"):
        c, r = call(side, "POST", f"/applications/{u[side]}/stop")
        record(f"[swarm] {side} stop accepted", c == 200, r)
    for _ in range(20):
        if not vm(e.vm2, f"docker stack services {u['ref']} -q; docker stack services {u['rs']} -q").strip():
            break
        time.sleep(3)
    for side in ("ref", "rs"):
        record(f"[swarm] {side} stack removed", not vm(e.vm2, f"docker stack services {u[side]} -q").strip())





COMPOSE = base64.b64encode(b"""services:
  web:
    image: nginx:alpine
    volumes:
      - webdata:/data
  cache:
    image: redis:7.2
    volumes:
      - cachedata:/data
""").decode()


def wait_sql(query, want, tries=60):
    for _ in range(tries):
        got = sql(query)
        if got == want:
            return got
        time.sleep(3)
    return sql(query)


def running(u, machine=VM1):
    return vm(machine, f"docker ps --format '{{{{.Names}}}}' --filter name={u}").split()


def case_resources(e, rs):
    """/deploy with services and databases, the public port of a service database, clones with volumes."""
    created = {}
    for side in ("ref", "rs"):
        c, r = call("ref", "POST", "/services", {"name": f"res-{side}", "project_uuid": e.project, "environment_name": "production",
                                                 "server_uuid": SERVER1, "docker_compose_raw": COMPOSE, "instant_deploy": False})
        record(f"[resources] service {side} created", c == 201, r)
        created[side] = r["uuid"]
        e.services.append(r["uuid"])
    # /deploy with a service.
    for side in ("ref", "rs"):
        c, r = call(side, "POST", f"/deploy?uuid={created[side]}")
        want = {"deployments": [{"message": f"Service res-{side} started. It could take a while, be patient.", "resource_uuid": created[side]}]}
        record(f"[resources] {side} /deploy with a service", c == 200 and r == want, r)
    for side in ("ref", "rs"):
        for _ in range(60):
            if len(running(created[side])) >= 2:
                break
            time.sleep(3)
        record(f"[resources] {side} service runs", len(running(created[side])) >= 2, running(created[side]))
    # /deploy with a database, a member token too.
    dbs = {}
    for side in ("ref", "rs"):
        c, r = call("ref", "POST", "/databases/redis", {"name": f"flush-{side}", "project_uuid": e.project, "environment_name": "production",
                                                        "server_uuid": SERVER1, "instant_deploy": False})
        record(f"[resources] redis {side} created", c == 200 or c == 201, r)
        dbs[side] = r["uuid"]
        e.databases.append(r["uuid"])
    for side in ("ref", "rs"):
        c, r = call(side, "POST", f"/deploy?uuid={dbs[side]}")
        want = {"deployments": [{"message": f"Database flush-{side} started.", "resource_uuid": dbs[side]}]}
        record(f"[resources] {side} /deploy with a database", c == 200 and r == want, r)
    for side in ("ref", "rs"):
        for _ in range(40):
            if running(dbs[side]):
                break
            time.sleep(3)
        record(f"[resources] {side} database runs", bool(running(dbs[side])))
        record(f"[resources] {side} started_at", sql(f"SELECT started_at IS NOT NULL FROM standalone_redis WHERE uuid='{dbs[side]}'") == "t")
    time.sleep(5)
    # Flush all data: coolify-rs through its route, upstream through FlushDatabase::run.
    for side in ("ref", "rs"):
        u = dbs[side]
        vm(VM1, f"docker exec {u} sh -c 'REDISCLI_AUTH=\"$REDIS_PASSWORD\" redis-cli SET flushme 1'")
        before = vm(VM1, f"docker exec {u} sh -c 'REDISCLI_AUTH=\"$REDIS_PASSWORD\" redis-cli DBSIZE'").strip()
        if side == "rs":
            code, b = rs.post(f"/ui/databases/{u}/flush", {"password": "wrong"})
            record("[resources] rs flush wrong password", code == 422 and b"incorrect" in b, b[:200])
            code, b = rs.post(f"/ui/databases/{u}/flush", {"password": rs.password})
            record("[resources] rs flush answer", code == 200 and b"Database flushed." in b, b[:200])
        else:
            subprocess.run(["docker", "exec", "coolify-rs-reference-coolify-1", "php", "/var/www/html/artisan", "tinker", "--execute",
                            f'App\\Actions\\Database\\FlushDatabase::run(App\\Models\\StandaloneRedis::where("uuid","{u}")->first());'], capture_output=True, text=True)
        time.sleep(1)
        after = vm(VM1, f"docker exec {u} sh -c 'REDISCLI_AUTH=\"$REDIS_PASSWORD\" redis-cli DBSIZE'").strip()
        record(f"[resources] {side} flush emptied the database", before == "1" and after == "0", (before, after))
    code, b = rs.post(f"/ui/databases/{created['rs']}/flush", {"password": rs.password})
    record("[resources] flush of something else is refused", code == 404, code)
    # Public port of a service database.
    for side in ("ref", "rs"):
        u = created[side]
        c, listed = call("ref", "GET", f"/services/{u}")
        cache = next(x for x in listed["databases"] if x["name"] == "cache")
        port = 15690 if side == "ref" else 15691
        c, r = call(side, "PATCH", f"/services/{u}/databases/{cache['uuid']}", {"is_public": True, "public_port": port})
        record(f"[resources] {side} service database public", c == 200 and r.get("is_public") is True, r)
        for _ in range(30):
            if str(port) in vm(VM1, "docker ps --format '{{.Ports}}'"):
                break
            time.sleep(2)
        record(f"[resources] {side} proxy on the port", str(port) in vm(VM1, "docker ps --format '{{.Ports}}'"))
        c, r = call(side, "PATCH", f"/services/{u}/databases/{cache['uuid']}", {"is_public": False})
        record(f"[resources] {side} service database private", c == 200 and r.get("is_public") is False, r)
        for _ in range(30):
            if str(port) not in vm(VM1, "docker ps --format '{{.Ports}}'"):
                break
            time.sleep(2)
        record(f"[resources] {side} proxy gone", str(port) not in vm(VM1, "docker ps --format '{{.Ports}}'"))
    # Clone with volumes: same server, then to the second server.
    dest1 = sql("SELECT uuid FROM standalone_dockers WHERE server_id=(SELECT id FROM servers WHERE uuid='" + SERVER1 + "') ORDER BY id LIMIT 1")
    dest2 = sql(f"SELECT uuid FROM standalone_dockers WHERE id={e.dest2}")
    for label, destination, machine in (("same server", dest1, VM1), ("second server", dest2, e.vm2)):
        for side in ("ref", "rs"):
            u = created[side]
            vm(VM1, f"docker run --rm -v {u}_webdata:/d alpine sh -c 'echo {side} > /d/marker.txt'")
            c, r = call(side, "POST", f"/services/{u}/clone", {"destination_uuid": destination, "clone_volumes": True, "name": f"clone-{side}"})
            record(f"[resources] {label}: {side} clone answer", c == 201 and r.get("message") == "Service cloned.", r)
            if c != 201:
                continue
            e.services.append(r["uuid"])
            new = r["uuid"]
            got = ""
            began = time.time()
            for _ in range(200):
                got = vm(machine, f"docker volume inspect {new}_webdata >/dev/null 2>&1 && docker run --rm -v {new}_webdata:/d alpine cat /d/marker.txt 2>/dev/null").strip()
                if got:
                    break
                time.sleep(3)
            print(f"  {label} {side}: copy seen after {int(time.time() - began)}s")
            record(f"[resources] {label}: {side} volume data copied", got == side, got)
            began = time.time()
            for _ in range(200):
                if len(running(u)) >= 2:
                    break
                time.sleep(3)
            print(f"  {label} {side}: source back after {int(time.time() - began)}s")
            record(f"[resources] {label}: {side} source started again", len(running(u)) >= 2, running(u))
            call("ref", "DELETE", f"/services/{new}?delete_volumes=true")
            e.cleanups.append((machine, f"docker volume rm -f {new}_webdata {new}_cachedata >/dev/null 2>&1"))
    # Clone of a database and of an application with a volume to the second server.
    u = dbs["rs"]
    vm(VM1, f"docker exec {u} sh -c 'REDISCLI_AUTH=\"$REDIS_PASSWORD\" redis-cli SET cloned 1; REDISCLI_AUTH=\"$REDIS_PASSWORD\" redis-cli SAVE'")
    for side in ("ref", "rs"):
        c, r = call(side, "POST", f"/databases/{dbs[side]}/clone", {"destination_uuid": dest2, "clone_volumes": True, "name": f"dbclone-{side}"})
        record(f"[resources] {side} database clone answer", c == 201, r)
        if c == 201:
            e.databases.append(r["uuid"])
            vols = ""
            for _ in range(50):
                vols = vm(e.vm2, f"docker volume ls -q --filter name={r['uuid']}")
                if vols.strip():
                    break
                time.sleep(3)
            record(f"[resources] {side} database volume on the second server", bool(vols.strip()), vols)
            for v in vols.split():
                e.cleanups.append((e.vm2, f"docker volume rm -f {v} >/dev/null 2>&1"))


def case_missing_destination(e):
    """A row whose destination is gone: the 500s of `$resource->destination->server`, and the delete that still works."""
    uuids = e.create_pair("nodest", image=False)
    dbs = {}
    for side in ("ref", "rs"):
        c, r = call(side, "POST", "/databases/redis", {"server_uuid": SERVER1, "project_uuid": e.project, "environment_name": "production", "name": f"nodest-{side}", "instant_deploy": False})
        record(f"[nodest] {side} redis created", c == 201, r)
        dbs[side] = r["uuid"]
        e.databases.append(r["uuid"])
    for side in ("ref", "rs"):
        sql(f"UPDATE applications SET destination_id = 999999 WHERE uuid = '{uuids[side]}'")
        sql(f"UPDATE standalone_redis SET destination_id = 999999 WHERE uuid = '{dbs[side]}'")

    def both(label, app):
        out = {}
        for side in ("ref", "rs"):
            kind, u = ("applications", uuids[side]) if app else ("databases", dbs[side])
            c, r = call(side, *label(kind, u))
            out[side] = (c, r if isinstance(r, dict) and "exception" not in r else (r.get("message") if isinstance(r, dict) else str(r)[:80]))
        record(f"[nodest] {'application' if app else 'database'} {label('x', 'y')[1].split('/')[-1]}", out["ref"] == out["rs"], out)

    for app in (True, False):
        for action in ("start", "restart", "stop"):
            both(lambda k, u, a=action: ("GET", f"/{k}/{u}/{a}"), app)
    for side in ("ref", "rs"):
        c, r = call(side, "DELETE", f"/applications/{uuids[side]}?delete_volumes=true")
        record(f"[nodest] {side} deletes the application", c == 200, r)
        c, r = call(side, "DELETE", f"/databases/{dbs[side]}?delete_volumes=true")
        record(f"[nodest] {side} deletes the database", c == 200, r)
    apps_gone = wait_sql("SELECT count(*) FROM applications WHERE uuid IN ('{}','{}')".format(uuids["ref"], uuids["rs"]), "0")
    record("[nodest] both application rows gone", apps_gone)
    dbs_gone = wait_sql("SELECT count(*) FROM standalone_redis WHERE uuid IN ('{}','{}')".format(dbs["ref"], dbs["rs"]), "0")
    record("[nodest] both database rows gone", dbs_gone)


def case_server_delete(e):
    """DELETE /servers/{uuid}?force=true on a server with a running database and a queued deployment."""
    ip = vm(e.vm2, "hostname -I | cut -d' ' -f1").strip()
    key = sql(f"SELECT uuid FROM private_keys WHERE id={KEY_ID}")
    result = {}
    for side in ("ref", "rs"):
        c, srv = call("ref", "POST", "/servers", {"name": f"sdel-{side}", "ip": ip, "port": 22, "user": "root", "private_key_uuid": key,
                                                  "instant_validate": False, "proxy_type": "none"})
        record(f"[server-delete] {side} server created", c == 201, srv)
        u = srv["uuid"]
        sid = sql(f"SELECT id FROM servers WHERE uuid='{u}'")
        sql(f"UPDATE server_settings SET is_reachable=true, is_usable=true WHERE server_id={sid}")
        if not sql(f"SELECT id FROM standalone_dockers WHERE server_id={sid}"):
            sql(f"INSERT INTO standalone_dockers (name, uuid, network, server_id, created_at, updated_at) VALUES ('sdel', 'sdel{os.getpid()}{side}', 'coolify', {sid}, now(), now())")
        dest = int(sql(f"SELECT id FROM standalone_dockers WHERE server_id={sid} LIMIT 1"))
        c, db = call(side, "POST", "/databases/redis", {"server_uuid": u, "project_uuid": e.project, "environment_name": "production", "name": f"sdel-{side}", "instant_deploy": True})
        record(f"[server-delete] {side} redis created", c == 201, db)
        e.cleanups.append((e.vm2, f"docker rm -f {db['uuid']} >/dev/null 2>&1; docker volume rm -f redis-data-{db['uuid']} >/dev/null 2>&1; rm -rf /data/coolify/databases/{db['uuid']}"))
        for _ in range(60):
            if vm(e.vm2, f"docker ps -q --filter name=^{db['uuid']}$").strip():
                break
            time.sleep(2)
        record(f"[server-delete] {side} redis running", bool(vm(e.vm2, f"docker ps -q --filter name=^{db['uuid']}$").strip()))
        # A deployment still queued for this server; the application row is the test server's own pair.
        app = e.create_pair(f"sdel-{side}", image=False)[side]
        app_id = sql(f"SELECT id FROM applications WHERE uuid='{app}'")
        sql(f"INSERT INTO application_deployment_queues (application_id, deployment_uuid, pull_request_id, force_rebuild, restart_only, status, server_id, destination_id, server_name, application_name, created_at, updated_at) "
            f"VALUES ('{app_id}', 'sdelq{os.getpid()}{side}', 0, false, false, 'queued', {sid}, {dest}, 'sdel-{side}', 'sdel-{side}', now(), now())")
        no_force = call(side, "DELETE", f"/servers/{u}")
        forced = call(side, "DELETE", f"/servers/{u}?force=true")
        gone = wait_sql(f"SELECT count(*) FROM servers WHERE uuid='{u}'", "0")
        db_gone = wait_sql(f"SELECT count(*) FROM standalone_redis WHERE uuid='{db['uuid']}'", "0")
        time.sleep(5)
        q = sql(f"SELECT status FROM application_deployment_queues WHERE deployment_uuid='sdelq{os.getpid()}{side}'")
        log = sql(f"SELECT logs FROM application_deployment_queues WHERE deployment_uuid='sdelq{os.getpid()}{side}'")
        result[side] = {"no_force": no_force, "forced": forced, "server_gone": gone, "database_gone": db_gone, "queued": q,
                        "log": [(x["output"], x["type"], x["hidden"]) for x in json.loads(log or "[]")],
                        "settings": sql(f"SELECT count(*) FROM server_settings WHERE server_id={sid}"), "dest": sql(f"SELECT count(*) FROM standalone_dockers WHERE server_id={sid}"),
                        "container_left": bool(vm(e.vm2, f"docker ps -aq --filter name=^{db['uuid']}$").strip()),
                        "config_left": vm(e.vm2, f"test -d /data/coolify/databases/{db['uuid']} && echo y").strip()}
        sql(f"DELETE FROM application_deployment_queues WHERE deployment_uuid='sdelq{os.getpid()}{side}'")
        print(f"[server-delete] {side}: {result[side]}")
    for key_ in result["ref"]:
        record(f"[server-delete] {key_}", result["ref"][key_] == result["rs"][key_], (result["ref"][key_], result["rs"][key_]))


def case_swarm_database(e):
    """Create and clone of a database, and a service, on a Swarm destination, through each side's API."""
    vm(e.vm2, "docker info --format '{{.Swarm.LocalNodeState}}' | grep -q active || docker swarm init >/dev/null 2>&1")
    sql(f"UPDATE server_settings SET is_swarm_manager=true WHERE server_id={e.server2_id}")
    if not e.swarm_dest:
        e.swarm_dest = f"msds{os.getpid()}"
        sql(f"INSERT INTO swarm_dockers (name, uuid, network, server_id, created_at, updated_at) VALUES ('multi-diff-swarm', '{e.swarm_dest}', 'coolify-overlay', {e.server2_id}, now(), now())")
    made = {}
    for side in ("ref", "rs"):
        base = {"server_uuid": e.server2, "project_uuid": e.project, "environment_name": "production", "name": f"swdb-{side}", "instant_deploy": False}
        c, r = call(side, "POST", "/databases/redis", base)
        record("[swarm-db] two destinations and no uuid answers alike", c == 400, r)
        c, r = call(side, "POST", "/databases/redis", dict(base, destination_uuid=e.swarm_dest))
        record(f"[swarm-db] {side} created", c == 201, r)
        made[side] = r["uuid"]
        e.databases.append(r["uuid"])
    q = "SELECT destination_type, destination_id FROM standalone_redis WHERE uuid='{}'"
    record("[swarm-db] rows alike", sql(q.format(made["ref"])) == sql(q.format(made["rs"])) and "SwarmDocker" in sql(q.format(made["rs"])), (sql(q.format(made["ref"])), sql(q.format(made["rs"]))))
    clones = {}
    for side in ("ref", "rs"):
        c, r = call(side, "POST", f"/databases/{made[side]}/clone", {"destination_uuid": e.swarm_dest, "name": f"swdb-clone-{side}"})
        clones[side] = (c, list(r) if isinstance(r, dict) else r)
        if c in (200, 201):
            e.databases.append(r["uuid"])
            record(f"[swarm-db] {side} clone row", "SwarmDocker" in sql(q.format(r["uuid"])), sql(q.format(r["uuid"])))
    record("[swarm-db] clone answers alike", clones["ref"] == clones["rs"], clones)
    vm(e.vm2, "docker pull redis:7.2 >/dev/null 2>&1")
    starts = {}
    for side in ("ref", "rs"):
        starts[side] = call(side, "GET", f"/databases/{made[side]}/start")
        e.cleanups.append((e.vm2, f"docker rm -f {made[side]} >/dev/null 2>&1; docker volume rm -f redis-data-{made[side]} >/dev/null 2>&1; rm -rf /data/coolify/databases/{made[side]}"))
    record("[swarm-db] start answers alike", starts["ref"] == starts["rs"], starts)
    time.sleep(25)
    for side in ("ref", "rs"):
        print(f"[swarm-db] {side} container:", vm(e.vm2, f"docker ps -a --filter name={made[side]} --format '{{{{.Names}}}} {{{{.Status}}}} {{{{.Networks}}}}'").strip()[:160])
    state = {side: vm(e.vm2, f"docker ps -a --filter name=^{made[side]}$ --format '{{{{.Networks}}}}' ").strip() for side in made}
    record("[swarm-db] container networks alike", state["ref"] == state["rs"], state)
    for side in ("ref", "rs"):
        call(side, "GET", f"/databases/{made[side]}/stop")
    time.sleep(10)
    # The services create takes the same destination rows.
    svc = {}
    for side in ("ref", "rs"):
        c, r = call(side, "POST", "/services", {"type": "whoami", "server_uuid": e.server2, "project_uuid": e.project, "environment_name": "production", "name": f"swsvc-{side}",
                                                "destination_uuid": e.swarm_dest, "instant_deploy": False})
        svc[side] = (c, sorted(r) if isinstance(r, dict) else r)
        if c == 201:
            e.services.append(r["uuid"])
    record("[swarm-db] service create answers alike", svc["ref"] == svc["rs"], svc)
    q = "SELECT destination_type FROM services WHERE uuid='{}'"
    types = [sql(q.format(u)) for u in e.services[-2:]]
    record("[swarm-db] service rows alike", len(types) == 2 and types[0] == types[1], types)
    sql(f"UPDATE server_settings SET is_swarm_manager=false WHERE server_id={e.server2_id}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--vm2", default="coolify-rs-agent-deploy")
    ap.add_argument("--case", default="registry,missing-image,multi,build-noconfig,build,resources,swarm,swarm-db,missing-destination,server-delete")
    ap.add_argument("--keep-server", action="store_true")
    a = ap.parse_args()
    e = Env(a)
    try:
        e.prepare()
        rs = Rs()
        for case in a.case.split(","):
            try:
                if case == "registry":
                    case_registry(e)
                elif case == "missing-image":
                    case_missing(e)
                elif case == "multi":
                    case_multi(e, rs)
                elif case == "build-noconfig":
                    case_build(e, False)
                elif case == "build":
                    case_build(e, True)
                elif case == "swarm":
                    case_swarm(e)
                elif case == "resources":
                    case_resources(e, rs)
                elif case == "missing-destination":
                    case_missing_destination(e)
                elif case == "server-delete":
                    case_server_delete(e)
                elif case == "swarm-db":
                    case_swarm_database(e)
            except Exception as ex:  # noqa: BLE001
                record(f"[{case}] ran to the end", False, repr(ex))
    finally:
        e.cleanup()
    print(f"{sum(results)}/{len(results)} passed")
    print("base", d.BASES["rs"])
    sys.exit(0 if all(results) else 1)


main()
