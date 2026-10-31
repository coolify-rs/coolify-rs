#!/usr/bin/env python3
"""`POST /deploy?tag=` (`DeployController::by_tags`) on the reference and on coolify-rs: the paths that
do not start a deployment. Usage: COOLIFY_RS_BASE=http://127.0.0.1:<port> python3 compat/diff/tags_deploy_diff.py"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from databases_diff import call, sql  # noqa: E402

failed = 0
sql("insert into tags (uuid, name, team_id, created_at, updated_at) values ('diffnoresources', 'diffempty', 4, now(), now()) on conflict do nothing")
try:
    for name, method, path, body in [
        ("empty tag", "POST", "/api/v1/deploy?tag=diffempty", None),
        ("unknown tag", "POST", "/api/v1/deploy?tag=nosuchtag", None),
        ("empty and unknown", "POST", "/api/v1/deploy?tag=nosuchtag,diffempty", None),
        ("only commas", "POST", "/api/v1/deploy?tag=,", None),
        ("tag and uuid", "POST", "/api/v1/deploy?tag=a&uuid=b", None),
        ("tag and pr", "POST", "/api/v1/deploy?tag=a&pr=1", None),
        ("tag in body", "POST", "/api/v1/deploy", {"tag": "diffempty"}),
    ]:
        a, b = call("ref", method, path, body), call("rs", method, path, body)
        ok = a[0] == b[0] and a[1] == b[1]
        print(("ok  " if ok else "DIFF"), name, "" if ok else f"\n  ref {a}\n  rs  {b}")
        failed += 0 if ok else 1
finally:
    sql("delete from tags where uuid = 'diffnoresources'")
sys.exit(1 if failed else 0)
