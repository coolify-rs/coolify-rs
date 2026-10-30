#!/usr/bin/env python3
"""Differential check of Docker image applications: upstream reference vs coolify-rs.

Same setup as databases_diff.py (reference on :18000, coolify-rs on :18080 against the
reference database, a validated server both can reach). Usage:

    applications_diff.py --server <server uuid> --vm <orbstack machine name>

It creates a project, compares create responses and validation errors, the rows each
side writes (application, settings, labels), show/list responses for several tokens,
then deploys one application through each implementation and compares the deployment
rows, the log entries (text, type, hidden), the Docker Compose file on the server,
the container environment and the HTTP answer through the proxy; then stop, restart
and delete. Everything it creates is deleted at the end. Exit status 1 when anything differs.
"""
import argparse
import base64
import difflib
import json
import os
import re
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from applications_fields_diff import Cases, run_image_create  # noqa: E402
from databases_diff import call, json_diff, record, results, same, sql  # noqa: E402


def norm(text, *pairs):
    """Replaces UUIDs, container timestamps and names."""
    for old, new in pairs:
        text = text.replace(old, new)
    return re.sub(r"-\d{8}T\d{6}", "-TS", text)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", required=True)
    ap.add_argument("--vm", required=True)
    a = ap.parse_args()

    def vm(cmd):
        return subprocess.run(["orb", "-m", a.vm, "-u", "root", "bash", "-c", cmd], capture_output=True, text=True).stdout

    def app_row(u):
        return json.loads(sql(f"SELECT row_to_json(x)::text FROM applications x WHERE uuid='{u}'") or "null")

    def settings_row(u):
        return json.loads(sql(f"SELECT row_to_json(s)::text FROM application_settings s WHERE application_id=(SELECT id FROM applications WHERE uuid='{u}')") or "null")

    def deployment_row(u):
        return json.loads(sql(f"SELECT row_to_json(q)::text FROM application_deployment_queues q WHERE application_id=(SELECT id::text FROM applications WHERE uuid='{u}') ORDER BY id DESC LIMIT 1") or "null")

    def wait_deployment(u):
        for _ in range(150):
            d = deployment_row(u)
            if d and d["status"] in ("finished", "failed"):
                return d
            time.sleep(2)
        return deployment_row(u)

    project = json.loads(call("ref", "POST", "/api/v1/projects", {"name": "applications-diff"})[1])["uuid"]
    base = {"server_uuid": a.server, "project_uuid": project, "environment_name": "production"}
    created = []
    try:
        # Create with domains, ports and settings; compare responses and rows.
        body = dict(base, name="diff <b>app</b>", description="d", docker_registry_image_name="nginx:1.27-alpine",
                    ports_exposes="80,8080", limits_memory="512m", health_check_path="/health",
                    is_force_https_enabled=False, redirect="www")
        sides = {}
        for side, domain in (("ref", "https://ref-diff.example.com,http://x-ref-diff.example.com:3000"),
                             ("rs", "https://rs-diff.example.com,http://x-rs-diff.example.com:3000")):
            c, r = call(side, "POST", "/api/v1/applications/dockerimage", dict(body, domains=domain), tok="acme_owner_root")
            sides[side] = (c, json.loads(r))
            created.append(json.loads(r).get("uuid"))
        (ca, ja), (cb, jb) = sides["ref"], sides["rs"]
        ua, ub = ja["uuid"], jb["uuid"]
        record("create status and shape", ca == cb == 201 and list(ja) == list(jb), f"{ca} {ja} / {cb} {jb}")
        record("create domains", ja["domains"].replace("ref-diff", "X") == jb["domains"].replace("rs-diff", "X"), f"{ja} / {jb}")
        skip = {"id", "uuid", "created_at", "updated_at", "last_online_at", "fqdn", "custom_labels", "config_hash",
                "domain_port_overrides", "manual_webhook_secret_github", "manual_webhook_secret_gitlab",
                "manual_webhook_secret_bitbucket", "manual_webhook_secret_gitea"}
        ra, rb = app_row(ua), app_row(ub)
        record("application rows alike", {k: v for k, v in ra.items() if k not in skip} == {k: v for k, v in rb.items() if k not in skip},
               {k: (ra[k], rb.get(k)) for k in ra if k not in skip and ra[k] != rb.get(k)})
        record("domain port overrides alike",
               json.dumps(ra["domain_port_overrides"]).replace("ref-diff", "X") == json.dumps(rb["domain_port_overrides"]).replace("rs-diff", "X"),
               f"{ra['domain_port_overrides']} / {rb['domain_port_overrides']}")
        sa, sb = settings_row(ua), settings_row(ub)
        for k in ("id", "application_id", "created_at", "updated_at"):
            sa.pop(k), sb.pop(k)
        record("settings rows alike", sa == sb, {k: (sa[k], sb.get(k)) for k in sa if sa[k] != sb.get(k)})
        la = norm(ra["custom_labels"], (ua, "U"), ("ref-diff", "X"))
        lb = norm(rb["custom_labels"], (ub, "U"), ("rs-diff", "X"))
        record("generated labels identical", la == lb, "\n".join(difflib.unified_diff(la.splitlines(), lb.splitlines(), lineterm="")))

        for i, b in enumerate([
            {}, dict(base), dict(base, docker_registry_image_name="Nginx"), dict(base, docker_registry_image_name="nginx:sha256:abc"),
            dict(base, docker_registry_image_name="nginx", ports_exposes="80a"), dict(base, docker_registry_image_name="nginx", fqdn="http://a.b"),
            dict(base, docker_registry_image_name="nginx", foo=1), dict(base, project_uuid="nope", docker_registry_image_name="nginx"),
            dict(base, server_uuid="nope", docker_registry_image_name="nginx"), dict(base, environment_name="nope", docker_registry_image_name="nginx"),
            dict(base, docker_registry_image_name="nginx", domains="ftp://a.example.com"), dict(base, docker_registry_image_name="nginx", redirect="x"),
            dict(base, docker_registry_image_name="nginx", ports_mappings="80:80,80:81"),
            {"server_uuid": a.server, "project_uuid": project, "docker_registry_image_name": "nginx"},
        ]):
            same(f"create error {i}", "POST", "/api/v1/applications/dockerimage", b)
        same("instant deploy without deploy ability", "POST", "/api/v1/applications/dockerimage",
             dict(base, docker_registry_image_name="nginx", instant_deploy=True))
        same("member cannot create", "POST", "/api/v1/applications/dockerimage", dict(base, docker_registry_image_name="nginx"),
             tok="acme_member_write_legacy")

        for tok in ("acme_owner_read", "acme_owner_root"):
            ra_ = call("ref", "GET", f"/api/v1/applications/{ua}", tok=tok)
            rb_ = call("rs", "GET", f"/api/v1/applications/{ua}", tok=tok)
            record(f"show [{tok}]", ra_ == rb_, json_diff(ra_, rb_))
        same("list [acme_owner_read]", "GET", "/api/v1/applications", tok="acme_owner_read")
        same("show missing", "GET", "/api/v1/applications/nope", tok="acme_owner_read")
        for action in ("start", "stop", "restart"):
            same(f"{action} with GET", "GET", f"/api/v1/applications/{ua}/{action}", tok="acme_owner_root")
            same(f"{action} without deploy", "POST", f"/api/v1/applications/{ua}/{action}")
        same("deploy with GET", "GET", f"/api/v1/deploy?uuid={ua}", tok="acme_owner_root")
        same("deploy errors", "POST", "/api/v1/deploy", {}, tok="acme_owner_root")
        same("deploy unknown uuid", "POST", "/api/v1/deploy", {"uuid": "nope"}, tok="acme_owner_root")
        same("deployment missing", "GET", "/api/v1/deployments/nope", tok="acme_owner_root")
        same("delete missing", "DELETE", "/api/v1/applications/nope", tok="acme_owner_root")

        # Every other create field on this type, and POST /applications/dockerfile.
        fields = Cases(base)
        try:
            run_image_create(fields)
        finally:
            fields.cleanup()

        # Deploy one plain application through each side.
        deployed = {}
        for side in ("ref", "rs"):
            c, r = call(side, "POST", "/api/v1/applications/dockerimage",
                        dict(base, name="deploy-diff", docker_registry_image_name="nginx", docker_registry_image_tag="alpine",
                             ports_exposes="80", instant_deploy=True), tok="acme_owner_root")
            u = json.loads(r)["uuid"]
            created.append(u)
            deployed[side] = (u, json.loads(r)["domains"], wait_deployment(u))
        (ur, dr, qr), (us, ds, qs) = deployed["ref"], deployed["rs"]
        record("both deployments finished", qr["status"] == qs["status"] == "finished", f"{qr['status']} / {qs['status']}")
        skip = {"id", "application_id", "deployment_uuid", "created_at", "updated_at", "finished_at", "deployment_url",
                "application_name", "horizon_job_id", "horizon_job_worker", "current_process_id", "logs", "configuration_snapshot",
                "configuration_hash", "configuration_diff"}
        record("deployment rows alike", {k: v for k, v in qr.items() if k not in skip} == {k: v for k, v in qs.items() if k not in skip},
               {k: (qr[k], qs.get(k)) for k in qr if k not in skip and qr[k] != qs.get(k)})

        def visible_log(q, u):
            entries = json.loads(q["logs"])
            return [(e["type"], norm(e["output"], (u, "U"), (q["deployment_uuid"], "DEP"))) for e in entries if not e["hidden"]]
        lr, ls = visible_log(qr, ur), visible_log(qs, us)
        record("visible log entries identical", lr == ls,
               "\n".join(difflib.unified_diff([f"{t} {o}" for t, o in lr], [f"{t} {o}" for t, o in ls], lineterm="")))
        record("log entry keys identical",
               [list(e) for e in json.loads(qr["logs"])][:1] == [list(e) for e in json.loads(qs["logs"])][:1])
        cr = norm(vm(f"cat /data/coolify/applications/{ur}/docker-compose.yaml"), (ur, "U"))
        cs = norm(vm(f"cat /data/coolify/applications/{us}/docker-compose.yaml"), (us, "U"))
        record("docker-compose.yaml identical (UUIDs normalized)", cr == cs and cr != "",
               "\n".join(difflib.unified_diff(cr.splitlines(), cs.splitlines(), lineterm="")))
        files_r = vm(f"ls -A /data/coolify/applications/{ur}").split()
        files_s = vm(f"ls -A /data/coolify/applications/{us}").split()
        record("configuration directory files alike", files_r == files_s, f"{files_r} / {files_s}")
        envs = []
        for u in (ur, us):
            name = vm(f"docker ps --filter label=coolify.applicationUuid={u} --format '{{{{.Names}}}}'").strip()
            env = json.loads(vm(f"docker inspect --format '{{{{json .Config.Env}}}}' {name}") or "[]")
            envs.append(sorted(norm(x, (u, "U")) for x in env))
        record("container environment identical", envs[0] == envs[1], set(envs[0]) ^ set(envs[1]))
        codes = []
        for u, domain in ((ur, dr), (us, ds)):
            host = domain.split("://", 1)[1]
            codes.append(vm(f"curl -s -o /dev/null -w '%{{http_code}}' -H 'Host: {host}' http://127.0.0.1/"))
        record("both answer through the proxy", codes == ["200", "200"], codes)
        for tok in ("acme_owner_read", "acme_owner_root"):
            a_ = call("ref", "GET", f"/api/v1/deployments/applications/{ur}", tok=tok)
            b_ = call("rs", "GET", f"/api/v1/deployments/applications/{ur}", tok=tok)
            record(f"deployments of an application [{tok}]", a_ == b_, json_diff(a_, b_))
            a_ = call("ref", "GET", f"/api/v1/deployments/{qr['deployment_uuid']}", tok=tok)
            b_ = call("rs", "GET", f"/api/v1/deployments/{qr['deployment_uuid']}", tok=tok)
            record(f"deployment by uuid [{tok}]", a_ == b_, json_diff(a_, b_))

        # PATCH with instant_deploy: a new deployment on each side.
        before = {u: deployment_row(u)["id"] for u in (ur, us)}
        patched = {}
        for side, u in (("ref", ur), ("rs", us)):
            c, r = call(side, "PATCH", f"/api/v1/applications/{u}", {"limits_memory": "300m", "instant_deploy": True}, tok="acme_owner_root")
            patched[side] = (c, r.replace(u.encode(), b"U"))
        record("patch with instant_deploy answers alike", patched["ref"] == patched["rs"] and patched["rs"][0] == 200, patched)
        for u in (ur, us):
            for _ in range(150):
                d = deployment_row(u)
                if d["id"] != before[u] and d["status"] in ("finished", "failed"):
                    break
                time.sleep(2)
            record(f"patch deployment finished {u[:6]}", d["id"] != before[u] and d["status"] == "finished", d["status"])
        limits = [app_row(u)["limits_memory"] for u in (ur, us)]
        record("patch saved before the deployment", limits == ["300m", "300m"], limits)

        # Stop through each side.
        for side, u in (("ref", ur), ("rs", us)):
            call(side, "POST", f"/api/v1/applications/{u}/stop", tok="acme_owner_root")
        time.sleep(15)
        left = [vm(f"docker ps -a --filter label=coolify.applicationUuid={u} --format '{{{{.Names}}}}'").strip() for u in (ur, us)]
        record("stop removed the containers", left == ["", ""], left)
        sr, ss = app_row(ur), app_row(us)
        record("status after stop alike", (sr["status"], sr["container_present"]) == (ss["status"], ss["container_present"]),
               f"{sr['status']} {sr['container_present']} / {ss['status']} {ss['container_present']}")

        # Delete through each side.
        for side, u in (("ref", ur), ("rs", us)):
            c, r = call(side, "DELETE", f"/api/v1/applications/{u}", tok="acme_owner_root")
            record(f"delete accepted [{side}]", c == 200, r)
        time.sleep(15)
        for u in (ur, us):
            record(f"rows removed {u[:6]}", sql(f"SELECT count(*) FROM applications WHERE uuid='{u}'") == "0")
            record(f"configuration removed {u[:6]}", vm(f"ls /data/coolify/applications/{u} 2>&1").strip().endswith("No such file or directory"))
    finally:
        for u in created:
            if u:
                call("ref", "DELETE", f"/api/v1/applications/{u}", tok="acme_owner_root")
        time.sleep(10)
        call("ref", "DELETE", f"/api/v1/projects/{project}", tok="acme_owner_root")
    passed = sum(results)
    print(f"\n{passed}/{len(results)} checks identical")
    sys.exit(0 if passed == len(results) else 1)


if __name__ == "__main__":
    main()
