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
        if len(q) >= count and all(x["status"] in END and x["finished_at"] and "Gracefully shutting down build container" in (x["logs"] or "") for x in q[:count]):
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
    record(f"{label}: commands", cr == cs, f"{len(cr)} vs {len(cs)}, first {first}")


class Env:
    def __init__(self, a):
        self.a = a
        self.vm2 = a.vm2
        self.server2 = None
        self.server2_id = None
        self.project = None
        self.apps = []
        self.dest_rows = []
        self.swarm_dest = None

    def prepare(self):
        """The registry and the second server."""
        vm(VM1, f"docker inspect {REGISTRY} >/dev/null 2>&1 || docker run -d --name {REGISTRY} --restart=no -p 5000:5000 registry:2")
        ip = vm(VM1, "hostname -I | cut -d' ' -f1").strip()
        vm(self.vm2, f"docker inspect {FORWARD} >/dev/null 2>&1 || docker run -d --name {FORWARD} --restart=no --network host alpine/socat tcp-listen:5000,fork,reuseaddr,bind=127.0.0.1 tcp-connect:{ip}:5000")
        vm(self.vm2, "docker pull nginx:alpine >/dev/null 2>&1")
        key = sql(f"SELECT uuid FROM private_keys WHERE id={KEY_ID}")
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
        for u in self.apps:
            call("ref", "DELETE", f"/applications/{u}?delete_volumes=true")
        for _ in range(60):
            if not self.apps or sql("SELECT count(*) FROM applications WHERE uuid IN ({})".format(",".join(f"'{u}'" for u in self.apps))) == "0":
                break
            time.sleep(3)
        for u in self.apps:
            for machine in (VM1, self.vm2):
                vm(machine, f"docker stack rm {u} >/dev/null 2>&1; docker ps -aq --filter label=coolify.applicationId --filter name={u} | xargs -r docker rm -f; docker network rm {u} >/dev/null 2>&1; rm -rf /data/coolify/applications/{u}")
        if self.project:
            for _ in range(10):
                if call("ref", "DELETE", f"/projects/{self.project}")[0] in (200, 404):
                    break
                time.sleep(3)
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
    return vm(machine, f"docker ps --format '{{{{.Names}}}}' --filter label=coolify.applicationId --filter name={u}").split()


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
        record(f"[{case}] build server recorded", all(int(q[0]["build_server_id"] or 0) == e.server2_id for q in (qr, qs)), [q[0]["build_server_id"] for q in (qr, qs)])
        if with_config:
            for side in ("ref", "rs"):
                record(f"[{case}] {side}: runs on the main server", bool(containers(VM1, u[side])) and not containers(e.vm2, u[side]))
                record(f"[{case}] {side}: helper gone from the build server", qr[0]["deployment_uuid"] not in vm(e.vm2, "docker ps -a --format '{{.Names}}'"))
    finally:
        sql(f"UPDATE server_settings SET server_role=NULL, is_build_server=false WHERE server_id={e.server2_id}")


def case_swarm(e):
    vm(e.vm2, "docker swarm init >/dev/null 2>&1; docker network inspect coolify-overlay >/dev/null 2>&1 || docker network create --driver overlay --attachable coolify-overlay")
    e.swarm_dest = f"msd{os.getpid()}swarm"
    sql(f"INSERT INTO swarm_dockers (name, uuid, network, server_id, created_at, updated_at) VALUES ('multi-diff-swarm', '{e.swarm_dest}', 'coolify-overlay', {e.server2_id}, now(), now())")
    sql(f"UPDATE server_settings SET is_swarm_manager=true WHERE server_id={e.server2_id}")
    sql(f"DELETE FROM standalone_dockers WHERE id={e.dest2} AND NOT EXISTS (SELECT 1 FROM additional_destinations WHERE standalone_docker_id={e.dest2})")
    u = e.create_pair("swarm", server=e.server2, destination_uuid=e.swarm_dest)
    for side in ("ref", "rs"):
        c, r = call(side, "GET", f"/applications/{u[side]}")
        record(f"[swarm] {side} destination", c == 200 and r.get("destination", {}).get("network") == "coolify-overlay", str(r)[:300])
    c1, a = call("ref", "GET", f"/applications/{u['ref']}")
    c2, b = call("rs", "GET", f"/applications/{u['ref']}")
    strip = ("updated_at",)
    record("[swarm] show alike", c1 == c2 and {k: v for k, v in a.items() if k not in strip} == {k: v for k, v in b.items() if k not in strip},
           {k: (a.get(k), b.get(k)) for k in a if a.get(k) != b.get(k)})
    qr, qs = deploy_pair(u)
    record("[swarm] both finished", qr and qs and qr[0]["status"] == qs[0]["status"] == "finished", (qr and qr[0]["status"], qs and qs[0]["status"], (qs and (qs[0]["logs"] or ""))[-500:]))
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
        record(f"[swarm] {side} service runs", "1/1" in s, s)
    for side in ("ref", "rs"):
        c, r = call(side, "POST", f"/applications/{u[side]}/stop")
        record(f"[swarm] {side} stop accepted", c == 200, r)
    for _ in range(20):
        if not vm(e.vm2, f"docker stack services {u['ref']} -q; docker stack services {u['rs']} -q").strip():
            break
        time.sleep(3)
    for side in ("ref", "rs"):
        record(f"[swarm] {side} stack removed", not vm(e.vm2, f"docker stack services {u[side]} -q").strip())


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--vm2", default="coolify-rs-agent-deploy")
    ap.add_argument("--case", default="registry,missing-image,multi,build-noconfig,build,swarm")
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
            except Exception as ex:  # noqa: BLE001
                record(f"[{case}] ran to the end", False, repr(ex))
    finally:
        e.cleanup()
    print(f"{sum(results)}/{len(results)} passed")
    print("base", d.BASES["rs"])
    sys.exit(0 if all(results) else 1)


main()
