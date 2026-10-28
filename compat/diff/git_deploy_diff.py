#!/usr/bin/env python3
"""Differential check of git deployments from a public repository: upstream vs coolify-rs.

Same setup as applications_diff.py. Usage:

    git_deploy_diff.py --server <server uuid> --vm <orbstack machine name> [--case dockerfile,static,nixpacks,railpack]

Per case it creates two identical applications from coollabsio/coolify-examples through
the reference API, deploys one with the reference and one with coolify-rs, and compares
the deployment rows, the visible log entries, the commands of the hidden entries, the
Docker Compose file, the container environment and the answer through the proxy. Then it
deploys both again (the build is skipped) and compares those logs too. Everything it
creates is deleted at the end. Exit status 1 when anything differs.
"""
import argparse
import difflib
import json
import os
import re
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from databases_diff import call, record, results, sql  # noqa: E402

REPO = "https://github.com/coollabsio/coolify-examples"
CASES = {
    "dockerfile": dict(build_pack="dockerfile", base_directory="/dockerfile/single-stage", ports_exposes="80"),
    "static": dict(build_pack="static", base_directory="/static", ports_exposes="80"),
    "nixpacks": dict(build_pack="nixpacks", base_directory="/javascript/runtimes/nodejs", ports_exposes="3000"),
    "railpack": dict(build_pack="railpack", base_directory="/javascript/runtimes/nodejs", ports_exposes="3000"),
    # A deploy key against a git-over-SSH mirror of the repository (--deploy-key, --ssh-repo).
    "deploykey": dict(build_pack="dockerfile", base_directory="/dockerfile/single-stage", ports_exposes="80"),
}


def norm(text, pairs):
    for old, new in pairs:
        if old:
            text = text.replace(old, new)
    text = re.sub(r"-\d{8}T\d{6}", "-TS", text)
    return text


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", required=True)
    ap.add_argument("--vm", required=True)
    ap.add_argument("--case", default=",".join(c for c in CASES if c != "deploykey"))
    ap.add_argument("--deploy-key", help="private key file the --ssh-repo accepts")
    ap.add_argument("--ssh-repo", help="e.g. git@192.168.139.220:examples.git")
    a = ap.parse_args()

    def vm(cmd):
        return subprocess.run(["orb", "-m", a.vm, "-u", "root", "bash", "-c", cmd], capture_output=True, text=True).stdout

    def deployments(u):
        rows = sql(f"SELECT row_to_json(q)::text FROM application_deployment_queues q WHERE application_id=(SELECT id::text FROM applications WHERE uuid='{u}') ORDER BY id")
        return [json.loads(r) for r in rows.splitlines() if r.strip()]

    def wait(u, count):
        for _ in range(300):
            d = deployments(u)
            if len(d) >= count and d[count - 1]["status"] in ("finished", "failed", "cancelled-by-user") and d[count - 1]["finished_at"]:
                # The finally block logs after finished_at.
                time.sleep(4)
                return deployments(u)[count - 1]
            time.sleep(2)
        return (deployments(u) or [None])[-1]

    def deploy(side, u):
        c, r = call(side, "POST", f"/api/v1/deploy?uuid={u}", {}, tok="acme_owner_root")
        return c, r

    project = json.loads(call("ref", "POST", "/api/v1/projects", {"name": "git-deploy-diff"})[1])["uuid"]
    base = {"server_uuid": a.server, "project_uuid": project, "environment_name": "production",
            "git_repository": REPO, "git_branch": "main", "instant_deploy": False}
    created = []
    key_uuid = None
    try:
        if "deploykey" in a.case.split(","):
            c, r = call("ref", "POST", "/api/v1/security/keys",
                        {"name": "git-deploy-diff", "private_key": open(a.deploy_key).read()}, tok="acme_owner_root")
            key_uuid = json.loads(r)["uuid"]
        for case in a.case.split(","):
            spec = CASES[case]
            apps = {}
            for side in ("ref", "rs"):
                if case == "deploykey":
                    body = dict(base, name=f"{case}-{side}", git_repository=a.ssh_repo, private_key_uuid=key_uuid, **spec)
                    c, r = call("ref", "POST", "/api/v1/applications/private-deploy-key", body, tok="acme_owner_root")
                else:
                    c, r = call("ref", "POST", "/api/v1/applications/public", dict(base, name=f"{case}-{side}", **spec), tok="acme_owner_root")
                body = json.loads(r)
                if c != 201:
                    record(f"[{case}] create", False, r)
                    break
                created.append(body["uuid"])
                apps[side] = body
            if len(apps) < 2:
                continue
            ur, us = apps["ref"]["uuid"], apps["rs"]["uuid"]
            for n in (1, 2):
                for side, u in (("ref", ur), ("rs", us)):
                    c, r = deploy(side, u)
                    if c != 200:
                        record(f"[{case}] deploy {n} accepted by {side}", False, f"{c} {r}")
                qr, qs = wait(ur, n), wait(us, n)
                label = f"[{case}] deploy {n}"
                record(f"{label}: both finished", qr and qs and qr["status"] == qs["status"] == "finished",
                       f"{qr and qr['status']} / {qs and qs['status']}")
                if not (qr and qs):
                    continue
                skip = {"id", "application_id", "deployment_uuid", "created_at", "updated_at", "finished_at", "deployment_url",
                        "application_name", "horizon_job_id", "horizon_job_worker", "current_process_id", "logs",
                        "configuration_snapshot", "configuration_hash", "configuration_diff", "is_api"}
                record(f"{label}: rows alike", {k: v for k, v in qr.items() if k not in skip} == {k: v for k, v in qs.items() if k not in skip},
                       {k: (qr[k], qs.get(k)) for k in qr if k not in skip and qr[k] != qs.get(k)})
                commit = qr.get("commit")

                def entries(q, u):
                    pairs = [(u, "U"), (q["deployment_uuid"], "DEP"), (commit, "SHA"), (f"{case}-ref", "NAME"), (f"{case}-rs", "NAME")]
                    out = json.loads(q["logs"] or "[]")
                    visible = [(e["type"], norm(e["output"], pairs)) for e in out if not e["hidden"]]
                    commands, seen = [], set()
                    for e in out:
                        cmd = e.get("command")
                        if cmd and (e["batch"], cmd) not in seen:
                            seen.add((e["batch"], cmd))
                            # The helper container id and hashed build args differ by run.
                            commands.append(re.sub(r"COOLIFY_BUILD_SECRETS_HASH=[0-9a-f]{64}", "COOLIFY_BUILD_SECRETS_HASH=H", norm(cmd, pairs)))
                    return visible, commands
                (vr, cr), (vs, cs) = entries(qr, ur), entries(qs, us)
                record(f"{label}: visible log entries identical", vr == vs,
                       "\n".join(difflib.unified_diff([f"{t} {o}" for t, o in vr], [f"{t} {o}" for t, o in vs], lineterm="")))
                record(f"{label}: logged commands identical", cr == cs,
                       "\n".join(difflib.unified_diff(cr, cs, lineterm="")))
            fr = norm(vm(f"cat /data/coolify/applications/{ur}/docker-compose.yaml"), [(ur, "U"), (f"{case}-ref", "NAME")])
            fs = norm(vm(f"cat /data/coolify/applications/{us}/docker-compose.yaml"), [(us, "U"), (f"{case}-rs", "NAME")])
            record(f"[{case}] docker-compose.yaml identical", fr == fs and fr != "",
                   "\n".join(difflib.unified_diff(fr.splitlines(), fs.splitlines(), lineterm="")))
            envs = []
            for u in (ur, us):
                name = vm(f"docker ps --filter label=coolify.applicationUuid={u} --format '{{{{.Names}}}}'").strip()
                env = json.loads(vm(f"docker inspect --format '{{{{json .Config.Env}}}}' {name}") or "[]")
                envs.append(sorted(norm(x, [(u, "U"), (f"{case}-ref", "NAME"), (f"{case}-rs", "NAME")]) for x in env))
            record(f"[{case}] container environment identical", envs[0] == envs[1], set(envs[0]) ^ set(envs[1]))
            codes = []
            for u in (ur, us):
                fqdn = json.loads(sql(f"SELECT to_json(fqdn)::text FROM applications WHERE uuid='{u}'") or "null") or ""
                host = fqdn.split("://", 1)[-1].split("/")[0]
                codes.append(vm(f"curl -s -o /dev/null -w '%{{http_code}}' -H 'Host: {host}' http://127.0.0.1/"))
            record(f"[{case}] both answer through the proxy", codes[0] == codes[1] == "200", codes)
            time.sleep(70)  # both status pollers run at least once
            ar = json.loads(sql(f"SELECT row_to_json(x)::text FROM applications x WHERE uuid='{ur}'"))
            as_ = json.loads(sql(f"SELECT row_to_json(x)::text FROM applications x WHERE uuid='{us}'"))
            keys = ["status", "custom_healthcheck_found", "health_check_interval", "health_check_timeout", "health_check_retries",
                    "health_check_start_period", "health_check_enabled", "config_hash"]
            record(f"[{case}] application health and status columns alike",
                   all(ar[k] == as_[k] for k in keys if k != "config_hash"), {k: (ar[k], as_[k]) for k in keys if k != "config_hash" and ar[k] != as_[k]})
    finally:
        for u in created:
            call("ref", "DELETE", f"/api/v1/applications/{u}?delete_volumes=true", tok="acme_owner_root")
        time.sleep(5)
        call("ref", "DELETE", f"/api/v1/projects/{project}", tok="acme_owner_root")
        if key_uuid:
            time.sleep(10)
            call("ref", "DELETE", f"/api/v1/security/keys/{key_uuid}", tok="acme_owner_root")
    passed = sum(results)
    print(f"\n{passed}/{len(results)} checks identical")
    sys.exit(0 if passed == len(results) else 1)


if __name__ == "__main__":
    main()
