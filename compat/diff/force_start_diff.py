#!/usr/bin/env python3
"""Differential check of force_start_deployment(): upstream (through tinker) vs coolify-rs (the UI route).

    force_start_diff.py --server <server uuid> [--track <file>]

Per side: one running deployment and a second one queued behind it. A force start moves the queued
row to in_progress although the application is busy, a repeated force start does nothing, and both
deployments end. Upstream's Livewire action has no HTTP API, so the reference is driven through the
helper it calls and coolify-rs through the route of its deployment page. Created rows are deleted.
"""
import argparse
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
from databases_diff import BASES, call, record, results, sql  # noqa: E402

REPO = "https://github.com/coollabsio/coolify-examples"
SECRETS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "reference", "secrets", "ui-login.txt")


def rs_session():
    """A logged-in opener and the CSRF token of the session."""
    text = open(SECRETS).read()
    password = re.search(r"^password:\s*(.+)$", text, re.M)[1].strip()
    opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))

    def post(path, body, csrf=""):
        req = urllib.request.Request(BASES["rs"] + path, data=json.dumps(body).encode(), method="POST")
        req.add_header("Content-Type", "application/json")
        req.add_header("Accept", "application/json")
        req.add_header("X-CSRF-Token", csrf)
        try:
            with opener.open(req) as r:
                return r.status, json.loads(r.read() or b"null")
        except urllib.error.HTTPError as e:
            return e.code, e.read()

    code, body = post("/ui/session", {"email": "alice@example.test", "password": password})
    if code != 200:
        raise SystemExit(f"login failed: {code}")
    return post, body["csrf_token"]


def ref_force_start(dep):
    code = ("$d = \\App\\Models\\ApplicationDeploymentQueue::where(\"deployment_uuid\", \"" + dep + "\")->first();"
            " echo \"started=\".(force_start_deployment($d) ? \"yes\" : \"no\");")
    out = subprocess.run(["docker", "exec", "coolify-rs-reference-coolify-1", "php", "/var/www/html/artisan", "tinker", "--execute", code],
                         capture_output=True, text=True).stdout
    m = re.search(r"started=(yes|no)", out)
    return m[1] == "yes" if m else None


def status(dep):
    return sql(f"SELECT status FROM application_deployment_queues WHERE deployment_uuid = '{dep}'")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", required=True)
    ap.add_argument("--track")
    a = ap.parse_args()
    post, csrf = rs_session()
    project = json.loads(call("ref", "POST", "/api/v1/projects", {"name": "force-start-diff"})[1])["uuid"]
    created = []
    seen = {}
    try:
        for side in ("ref", "rs"):
            body = {"server_uuid": a.server, "project_uuid": project, "environment_name": "production", "git_repository": REPO,
                    "git_branch": "main", "instant_deploy": False, "name": f"force-{side}", "build_pack": "dockerfile",
                    "base_directory": "/dockerfile/single-stage", "ports_exposes": "80"}
            code, r = call("ref", "POST", "/api/v1/applications/public", body, tok="acme_owner_root")
            u = json.loads(r)["uuid"]
            created.append(u)
            if a.track:
                with open(a.track, "a") as f:
                    f.write(u + "\n")
            deps = []
            for path in (f"/api/v1/deploy?uuid={u}&force=true", f"/api/v1/deploy?uuid={u}&force=true"):
                code, r = call(side, "POST", path, {}, tok="acme_owner_root")
                deps.append(json.loads(r)["deployments"][0]["deployment_uuid"])
            first, second = deps
            before = (status(first), status(second))

            def force(dep):
                if side == "ref":
                    return ref_force_start(dep)
                code, r = post(f"/ui/applications/{u}/deployments/{dep}/force-start", {}, csrf)
                return r["started"] if code == 200 else code

            started = force(second)
            after = status(second)
            again = force(second)
            for _ in range(240):
                if status(first) not in ("queued", "in_progress") and status(second) not in ("queued", "in_progress"):
                    break
                time.sleep(2)
            finished_again = force(second)
            seen[side] = {"before": before, "started": started, "after": after, "again": again, "finished_again": finished_again}
            if side == "rs":
                code, _ = post(f"/ui/applications/{u}/deployments/nosuch/force-start", {}, csrf)
                record("[rs] an unknown deployment answers 404", code == 404, code)
                code, _ = post(f"/ui/applications/{u}/deployments/{second}/force-start", {}, "wrong")
                record("[rs] the route checks the CSRF token", code != 200, code)
        record("force start behaves alike", seen["ref"] == seen["rs"], seen)
        record("the queued row started and a repeat did not", seen["ref"]["before"] == ("in_progress", "queued") and seen["ref"]["started"] is True
               and seen["ref"]["after"] == "in_progress" and seen["ref"]["again"] is False and seen["ref"]["finished_again"] is False, seen["ref"])
    finally:
        for u in created:
            call("ref", "DELETE", f"/api/v1/applications/{u}?delete_volumes=true", tok="acme_owner_root")
        for _ in range(120):
            if not created or sql("SELECT count(*) FROM applications WHERE uuid IN ({})".format(", ".join(f"'{u}'" for u in created))) == "0":
                break
            time.sleep(5)
        call("ref", "DELETE", f"/api/v1/projects/{project}", tok="acme_owner_root")
    passed = sum(results)
    print(f"\n{passed}/{len(results)} checks identical")
    sys.exit(0 if passed == len(results) else 1)


if __name__ == "__main__":
    main()
