#!/usr/bin/env python3
"""GET /projects/{uuid}/{environment} with resources, both sides, key order included.

Reads the test project's production environment, then creates a project with one service to
cover the services relation, and deletes exactly that. Set COOLIFY_RS_BASE to aim at an instance.
"""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import json  # noqa: E402

from databases_diff import call as raw_call, record, results  # noqa: E402


def call(side, method, path, body=None, tok="acme_owner_root"):
    st, b = raw_call(side, method, "/api/v1" + path, body, tok)
    try:
        return st, json.loads(b)
    except ValueError:
        return st, b.decode(errors="replace")


SERVER = "hml20fsj5eles0xfiqidq8j5"
ROOT_TOK = "acme_owner_root"


def diff(a, b, path=""):
    out = []
    if isinstance(a, dict) and isinstance(b, dict):
        if list(a.keys()) != list(b.keys()):
            sa, sb = set(a), set(b)
            out.append(f"{path} keys only-ref={sorted(sa - sb)[:8]} only-rs={sorted(sb - sa)[:8]} first={next(((x, y) for x, y in zip(a, b) if x != y), None)}")
        for k in a:
            if k in b:
                out += diff(a[k], b[k], f"{path}.{k}")
    elif isinstance(a, list) and isinstance(b, list):
        if len(a) != len(b):
            out.append(f"{path} len {len(a)} vs {len(b)}")
        for i, (x, y) in enumerate(zip(a, b)):
            out += diff(x, y, f"{path}[{i}]")
    elif a != b and not (".server." in path and path.rsplit(".", 1)[-1].endswith("_at")):
        # The scheduler of either side touches the shared server row between the two requests.
        out.append(f"{path}: {str(a)[:70]!r} vs {str(b)[:70]!r}")
    return out


def compare(name, path, tok):
    sr, a = call("ref", "GET", path, tok=tok)
    st, b = call("rs", "GET", path, tok=tok)
    # The relations have no ORDER BY upstream: heap order, so compare them by id.
    for side in (a, b):
        if isinstance(side, dict):
            for rel in ["applications", "postgresqls", "redis", "mongodbs", "mysqls", "mariadbs", "services"]:
                if isinstance(side.get(rel), list):
                    side[rel].sort(key=lambda r: r.get("id", 0))
    d = diff(a, b)
    record(name, sr == st and not d, f"{sr}/{st} {len(d)} differences: " + " ; ".join(d[:6]))


def main():
    for tok in [ROOT_TOK, "acme_owner_read", "acme_member_read"]:
        compare(f"test environment by name [{tok}]", "/projects/j8xm2emlllhfnyebjzgyzp7z/production", tok)
    compare("by uuid", "/projects/j8xm2emlllhfnyebjzgyzp7z/uj3st9yvtvzxh7rul3c3opcz", ROOT_TOK)
    compare("unknown environment", "/projects/j8xm2emlllhfnyebjzgyzp7z/nope", ROOT_TOK)
    project = None
    try:
        _, p = call("ref", "POST", "/projects", {"name": "envdetails-diff"}, tok=ROOT_TOK)
        project = p["uuid"]
        st, s = call("ref", "POST", "/services", {
            "type": "uptime-kuma", "name": "envdetails", "project_uuid": project,
            "environment_name": "production", "server_uuid": SERVER, "instant_deploy": False,
        }, tok=ROOT_TOK)
        record("service created", st == 201, str(s)[:200])
        for tok in [ROOT_TOK, "acme_owner_read"]:
            compare(f"with a service [{tok}]", f"/projects/{project}/production", tok)
        if st == 201:
            call("ref", "DELETE", f"/services/{s['uuid']}", tok=ROOT_TOK)
            time.sleep(3)
    finally:
        if project:
            for _ in range(10):
                st, _b = call("ref", "DELETE", f"/projects/{project}", tok=ROOT_TOK)
                if st in (200, 404):
                    break
                time.sleep(3)
    print(f"{sum(results)}/{len(results)} passed")
    sys.exit(0 if all(results) else 1)


main()
