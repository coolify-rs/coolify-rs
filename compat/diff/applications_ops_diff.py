#!/usr/bin/env python3
"""Differential check of the application operations: previews, clone, rollback, logs, move, migrate
and additional destinations.

Requests go to both implementations. Where an operation changes data each side has its own
application; clones are made by both sides from ONE source application, so the rows they create can be
compared byte for byte, encrypted columns included. Logs and rollback images read one running
application through both. Everything created is tracked by uuid or id in a state file and deleted at
the end; `--cleanup <state file>` finishes a run that crashed. Usage:

    COOLIFY_RS_BASE=http://127.0.0.1:18082 applications_ops_diff.py \
        --server hml20fsj5eles0xfiqidq8j5 --vm coolify-rs-e2e --state /tmp/ops-state.json

Exit status 1 when anything differs.
"""
import argparse
import base64
import json
import os
import re
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import applications_subresources_diff as sub  # noqa: E402
from applications_subresources_diff import (MEMBER_READ, MEMBER_WRITE, OTHER, READ, ROOT_TOK, SIDES, WRITE, both, call, cross_read, ctx,  # noqa: E402
                                            fill, parsed, tinker)
from databases_diff import record, results, sql  # noqa: E402

REPO = "https://github.com/coollabsio/coolify-examples"
COMPOSE = "services:\n  web:\n    image: nginx\n    volumes:\n      - data:/data\n  api:\n    image: nginx\n  my.db:\n    image: postgres:16\nvolumes:\n  data: {}\n"
COMPOSE_NO_VOLUME = "services:\n  web:\n    image: nginx\n  api:\n    image: nginx\n  my.db:\n    image: postgres:16\n"
EDGE_DEST = "q82uksnxswe56tw7oaumla53"
PRIMARY_DEST = "jjxfu7mlvsrgoxhwrqhrj4yd"
RUN = str(int(time.time()))
sub.UUIDISH |= {"deployment_uuid"}
_values_only = sub.norm


def _norm_keys(side, v):
    """`domain_port_overrides` has the side's domains as keys."""
    if isinstance(v, dict):
        return {_values_only(side, k): _norm_keys(side, x) for k, x in v.items()}
    if isinstance(v, list):
        return [_norm_keys(side, x) for x in v]
    return v


sub.norm = lambda side, v: _norm_keys(side, _values_only(side, v))
state = {"apps": [], "project": None, "volumes": [], "tags": [], "docker": [], "swarm": [], "tokens": [], "queue": []}
args = None
PREVIEW_DROP = ("id", "uuid", "application_id", "created_at", "updated_at", "last_online_at")
QUEUE_DROP = ("id", "application_id", "deployment_uuid", "created_at", "updated_at", "logs", "current_process_id", "horizon_job_id", "horizon_job_worker",
              "finished_at", "configuration_hash", "configuration_snapshot", "configuration_diff", "commit_message", "status")


def row_differences(a, b):
    """Only the columns that differ: application rows are long."""
    if not (isinstance(a, list) and isinstance(b, list)) or len(a) != len(b):
        return f"ref {a}\n  rs  {b}"
    out = []
    for i, (x, y) in enumerate(zip(a, b)):
        if not (isinstance(x, dict) and isinstance(y, dict)):
            out.append(f"{i}: {x} | {y}")
            continue
        for k in dict.fromkeys([*x, *y]):
            if x.get(k) != y.get(k):
                out.append(f"{i}.{k}: {first_difference(str(x.get(k)), str(y.get(k)))}")
    return "\n  ".join(out)


def same_rows(name, query, *drop):
    r = sub.rows(query, *drop)
    record(name, r["ref"] == r["rs"], row_differences(r["ref"], r["rs"]))


def save():
    with open(args.state, "w") as f:
        json.dump(state, f)


def vm(cmd):
    return subprocess.run(["orb", "-m", args.vm, "-u", "root", "bash", "-c", cmd], capture_output=True, text=True).stdout.strip()


def track(uuid, side="ref"):
    state["apps"].append({"side": side, "uuid": uuid, "id": sql(f"SELECT id FROM applications WHERE uuid='{uuid}'")})
    save()


def make(side, path, body):
    status, raw = call(side, "POST", "/api/v1/applications/" + path, fill(side, body), ROOT_TOK)
    if status != 201:
        raise SystemExit(f"{side}: could not create the application: {status} {raw[:400]}")
    uuid = json.loads(raw)["uuid"]
    track(uuid, side)
    return uuid


def each_side(key, path, body):
    """One application per side; `<key>` and `<key_id>` in paths and queries."""
    for side in SIDES:
        uuid = make(side, path, body)
        ctx[side][key] = uuid
        ctx[side][key + "_id"] = sql(f"SELECT id FROM applications WHERE uuid='{uuid}'")


def both_track(name, method, path, body=None, tok=ROOT_TOK, key=None):
    out = both(name, method, path, body, tok)
    for side in SIDES:
        if isinstance(out[side], dict) and out[side].get("uuid") and key:
            uuid = out[side]["uuid"]
            track(uuid, side)
            ctx[side][key] = uuid
            ctx[side][key + "_id"] = sql(f"SELECT id FROM applications WHERE uuid='{uuid}'")
    return out


def wait(what, check, seconds=240):
    for _ in range(seconds // 3):
        if check():
            return True
        time.sleep(3)
    print(f"timed out waiting for {what}")
    return False


def deployment_status(uuid):
    return sql(f"SELECT status FROM application_deployment_queues WHERE deployment_uuid='{uuid}'")


def wait_deployment(uuid):
    wait(f"deployment {uuid}", lambda: deployment_status(uuid) not in ("queued", "in_progress"))
    return deployment_status(uuid)


def queue_rows(name, condition):
    same_rows(name, f"SELECT * FROM application_deployment_queues WHERE {condition} ORDER BY id", *QUEUE_DROP)


def tiers(prefix, method, path, body=None, writes=True):
    """The answers for tokens without the right, of another team, and for an unknown application.
    `writes=False` when a write token would succeed and change what the following checks expect."""
    for label, tok in [("read token", READ), ("write token", WRITE), ("member read", MEMBER_READ), ("member legacy", MEMBER_WRITE), ("other team", OTHER)]:
        if writes or tok != WRITE:
            both(f"{prefix}: {label}", method, path, body, tok)


# ---------------------------------------------------------------------------


def move_cases():
    M = "/applications/<mv>/move"
    both("move: unknown app", "POST", "/applications/nope/move", {"environment_uuid": "x"})
    tiers("move", "POST", M, {"environment_uuid": "x"})
    both("move: empty", "POST", M, {})
    both("move: not string", "POST", M, {"environment_uuid": 1})
    both("move: extra", "POST", M, {"environment_uuid": "x", "foo": 1, "bar": 2})
    both("move: missing and extra", "POST", M, {"foo": 1})
    both("move: unknown environment", "POST", M, {"environment_uuid": "nope"})
    both("move: same environment", "POST", M, {"environment_uuid": "<env_prod>"})
    both("move: other team environment", "POST", M, {"environment_uuid": "<env_prod>"}, OTHER)
    both("move: success", "POST", M, {"environment_uuid": "<env_target>"})
    same_rows("move: environment of the row", "SELECT environment_id FROM applications WHERE id=<mv_id>")
    both("move: again", "POST", M, {"environment_uuid": "<env_target>"})
    both("move: back", "POST", M, {"environment_uuid": "<env_prod>"})


def migrate_cases():
    M = "/applications/<mv>/migrate"
    both("migrate: unknown app", "POST", "/applications/nope/migrate", {"destination_uuid": "x"})
    tiers("migrate", "POST", M, {"destination_uuid": PRIMARY_DEST})
    both("migrate: empty", "POST", M, {})
    both("migrate: valid (not a dev instance)", "POST", M, {"destination_uuid": EDGE_DEST})
    same_rows("migrate: destination unchanged", "SELECT destination_id FROM applications WHERE id=<mv_id>")


def destination_cases():
    extra_destinations(True)
    try:
        destination_checks()
    finally:
        extra_destinations(False)


def destination_checks():
    D = "/applications/<ds>/destinations"
    both("destinations: unknown app", "GET", "/applications/nope/destinations")
    both("destinations: list", "GET", D)
    both("destinations: list read token", "GET", D, tok=READ)
    both("destinations: list member", "GET", D, tok=MEMBER_READ)
    both("destinations: list other team", "GET", D, tok=OTHER)
    both("destinations add: unknown app", "POST", "/applications/nope/destinations", {"destination_uuid": "x"})
    tiers("destinations add", "POST", D, {"destination_uuid": EDGE_DEST}, writes=False)
    both("destinations add: empty", "POST", D, {})
    both("destinations add: not string", "POST", D, {"destination_uuid": 1})
    both("destinations add: extra", "POST", D, {"destination_uuid": EDGE_DEST, "foo": 1})
    both("destinations add: unknown", "POST", D, {"destination_uuid": "nope"})
    both("destinations add: primary", "POST", D, {"destination_uuid": PRIMARY_DEST})
    both("destinations add: same server", "POST", "/applications/<eg>/destinations", {"destination_uuid": "<edge_second_dest>"})
    both("destinations add: swarm destination", "POST", D, {"destination_uuid": "<swarm_dest>"})
    both("destinations add: compose", "POST", "/applications/<cp>/destinations", {"destination_uuid": EDGE_DEST})
    both("destinations add: persistent storage", "POST", "/applications/<ps>/destinations", {"destination_uuid": EDGE_DEST})
    both("destinations add: attach", "POST", D, {"destination_uuid": EDGE_DEST})
    pivot = "SELECT server_id, status, standalone_docker_id, created_at, updated_at FROM additional_destinations WHERE application_id=<ds_id> ORDER BY id"
    same_rows("destinations: pivot row", pivot)
    both("destinations: list with additional", "GET", D)
    both("destinations add: already attached", "POST", D, {"destination_uuid": EDGE_DEST})
    both("destinations add: other on that server", "POST", D, {"destination_uuid": "<edge_second_dest>"})
    tiers("destinations remove", "DELETE", D + "/" + EDGE_DEST, writes=False)
    both("destinations remove: unknown app", "DELETE", "/applications/nope/destinations/" + EDGE_DEST)
    both("destinations remove: unknown", "DELETE", D + "/nope")
    both("destinations remove: primary", "DELETE", D + "/" + PRIMARY_DEST)
    both("destinations remove: not attached", "DELETE", D + "/<edge_second_dest>")
    both("destinations remove: detach", "DELETE", D + "/" + EDGE_DEST)
    same_rows("destinations: pivot gone", pivot)
    both("destinations: list after detach", "GET", D)


def preview_validation(P):
    both("previews: unknown app", "GET", "/applications/nope/previews")
    both("previews: list empty", "GET", P)
    both("previews: list read", "GET", P, tok=READ)
    both("previews: list member", "GET", P, tok=MEMBER_READ)
    both("previews: list other team", "GET", P, tok=OTHER)
    both("previews show: unknown app", "GET", "/applications/nope/previews/1")
    both("previews show: not found", "GET", P + "/9")
    for bad in ["0", "abc", "2147483648", "-1", "1a", "99999999999999999999"]:
        both(f"previews show: invalid {bad}", "GET", f"{P}/{bad}")
        both(f"previews patch: invalid {bad}", "PATCH", f"{P}/{bad}", {"domains": None})
        both(f"previews delete: invalid {bad}", "DELETE", f"{P}/{bad}")
        both(f"previews logs: invalid {bad}", "GET", f"{P}/{bad}/logs")
    both("previews store: unknown app", "POST", "/applications/nope/previews", {"pull_request_id": 1})
    tiers("previews store", "POST", P, {"pull_request_id": 1, "docker_tag": "1", "instant_deploy": False})
    for name, body in [
        ("empty", {}),
        ("no tag", {"pull_request_id": 1}),
        ("zero", {"pull_request_id": 0, "docker_tag": "1"}),
        ("negative", {"pull_request_id": -1, "docker_tag": "1"}),
        ("too big", {"pull_request_id": 2147483648, "docker_tag": "1"}),
        ("not integer", {"pull_request_id": "x", "docker_tag": "1"}),
        ("float", {"pull_request_id": 1.5, "docker_tag": "1"}),
        ("bad url", {"pull_request_id": 1, "docker_tag": "1", "pull_request_html_url": "nope"}),
        ("long url", {"pull_request_id": 1, "docker_tag": "1", "pull_request_html_url": "https://a.example.com/" + "a" * 2050}),
        ("force", {"pull_request_id": 1, "docker_tag": "1", "force": "x"}),
        ("instant", {"pull_request_id": 1, "docker_tag": "1", "instant_deploy": "x"}),
        ("git type on image", {"pull_request_id": 1, "docker_tag": "1", "git_type": "github"}),
        ("null git type on image", {"pull_request_id": 1, "docker_tag": "1", "git_type": None}),
        ("commit on image", {"pull_request_id": 1, "docker_tag": "1", "commit": "abcdef0"}),
        ("bad tag", {"pull_request_id": 1, "docker_tag": "-bad"}),
        ("tag not string", {"pull_request_id": 1, "docker_tag": 5}),
        ("several", {"pull_request_id": "x", "pull_request_html_url": "nope", "force": "x", "git_type": "github", "docker_tag": "-x"}),
    ]:
        both(f"previews store image: {name}", "POST", P, body)


PREVIEW_ROWS = "SELECT * FROM application_previews WHERE application_id=<pv_id> ORDER BY pull_request_id"


def preview_cases():
    P = "/applications/<pv>/previews"
    preview_validation(P)
    both("previews store: create", "POST", P, {"pull_request_id": 5, "docker_tag": "1.27-alpine", "pull_request_html_url": "https://example.com/pull/5", "instant_deploy": False, "foo": 1})
    same_rows("previews: row created", PREVIEW_ROWS, *PREVIEW_DROP)
    both("previews store: second with string id", "POST", P, {"pull_request_id": "6", "docker_tag": "1.27-alpine", "instant_deploy": False})
    both("previews store: update existing", "POST", P, {"pull_request_id": 5, "pull_request_html_url": "https://example.com/pull/55", "docker_tag": "1.26-alpine", "instant_deploy": False})
    same_rows("previews: row updated", PREVIEW_ROWS, *PREVIEW_DROP)
    both("previews store: update without changes", "POST", P, {"pull_request_id": 5, "instant_deploy": False})
    both("previews: list", "GET", P)
    both("previews: show", "GET", P + "/5")
    both("previews: show read token", "GET", P + "/5", tok=READ)
    both("previews: show member", "GET", P + "/5", tok=MEMBER_READ)
    # Domains.
    tiers("previews patch", "PATCH", P + "/5", {"domains": None})
    both("previews patch: unknown app", "PATCH", "/applications/nope/previews/5", {"domains": None})
    both("previews patch: not found", "PATCH", P + "/9", {"domains": None})
    for name, body in [
        ("no field", {}),
        ("compose field", {"domains": None, "docker_compose_domains": []}),
        ("number", {"domains": 5}),
        ("array", {"domains": ["x"]}),
        ("invalid url", {"domains": "nope"}),
        ("two invalid", {"domains": "nope,ftp://x.example.com"}),
        ("too long", {"domains": "https://a.example.com/" + "a" * 2050}),
        ("force not bool", {"domains": None, "force_domain_override": "x"}),
        ("duplicates", {"domains": "https://<pvhost2>,https://<pvhost2>:8080"}),
    ]:
        both(f"previews patch: {name}", "PATCH", P + "/5", body)
    both("previews patch: with port", "PATCH", P + "/5", {"domains": "https://<pvhost2>:8081, https://www.<pvhost2>"})
    same_rows("previews: domains row", PREVIEW_ROWS, *PREVIEW_DROP)
    both("previews: show with overrides", "GET", P + "/5")
    both("previews patch: taken by an application", "PATCH", P + "/6", {"domains": "https://<shared_dom>"})
    both("previews patch: taken by an application, forced", "PATCH", P + "/6", {"domains": "https://<shared_dom>", "force_domain_override": True})
    same_rows("previews: forced domain row", PREVIEW_ROWS, *PREVIEW_DROP)
    both("previews patch: taken by this application's other preview", "PATCH", P + "/5", {"domains": "https://<shared_dom>"})
    both("previews patch: clear", "PATCH", P + "/6", {"domains": None})
    same_rows("previews: cleared row", PREVIEW_ROWS, *PREVIEW_DROP)
    # A real queued deployment: the same row, in_progress on both sides at first. The job differs.
    out = both("previews store: queue a deployment", "POST", P, {"pull_request_id": 7, "docker_tag": "1.27-alpine", "force": True})
    for side in SIDES:
        ctx[side]["dep"] = out[side].get("deployment_uuid")
    queue_rows("previews: queue row", "application_id='<pv_id>' AND pull_request_id=7")
    same_rows("previews: row of the queued preview", PREVIEW_ROWS, *PREVIEW_DROP)
    rs_status = wait_deployment(ctx["rs"]["dep"])
    record("previews: the rs job refuses the row instead of deploying production", rs_status == "failed",
           f"status {rs_status}")
    log = sql(f"SELECT logs FROM application_deployment_queues WHERE deployment_uuid='{ctx['rs']['dep']}'")
    record("previews: the rs row says why", "not ported" in log, log[:300])
    record("previews: rs started no container for the preview", vm(f"docker ps -a --filter label=coolify.applicationUuid={ctx['rs']['pv']} --format '{{{{.Names}}}}'") == "", "")
    # The reference's job really runs; wait for it so that deletion does not race the worker.
    wait_deployment(ctx["ref"]["dep"])
    print("  reference preview deployment:", deployment_status(ctx["ref"]["dep"]))
    # Preview logs: the container the reference ran, through both implementations.
    pr_logs = fill("ref", "/api/v1/applications/<pv>/previews/7/logs")
    a, b = call("ref", "GET", pr_logs, None, ROOT_TOK), call("rs", "GET", pr_logs, None, ROOT_TOK)
    record("previews: logs of the running preview", a == b, f"ref {a}\n  rs  {b}")
    both("previews: logs not found", "GET", P + "/9/logs")
    # The same deployment while one is running is skipped. The rs job fails the row at once, so a
    # running row is written by hand for both sides.
    both("previews store: for the skip check", "POST", P, {"pull_request_id": 9, "docker_tag": "1.27-alpine", "instant_deploy": False})
    for side in SIDES:
        uuid = f"opsdiff-{RUN}-{side}-running"
        state["queue"].append(uuid)
        save()
        sql(fill(side, f"INSERT INTO application_deployment_queues (application_id, deployment_uuid, pull_request_id, commit, status, docker_registry_image_tag, server_id, created_at, updated_at) "
                       f"VALUES ('<pv_id>', '{uuid}', 9, 'HEAD', 'in_progress', '1.27-alpine', 57, now(), now())"))
    both("previews store: same deployment is skipped", "POST", P, {"pull_request_id": 9, "docker_tag": "1.27-alpine"})
    same_rows("previews: no second row", "SELECT count(*) FROM application_deployment_queues WHERE application_id='<pv_id>' AND pull_request_id=9")
    for side in SIDES:
        sql(f"DELETE FROM application_deployment_queues WHERE deployment_uuid='opsdiff-{RUN}-{side}-running'")
    both("previews delete: the skip check", "DELETE", P + "/9")
    # Deletion.
    both("previews delete: unknown app", "DELETE", "/applications/nope/previews/5")
    tiers("previews delete", "DELETE", P + "/5")
    both("previews delete: not found", "DELETE", P + "/9")
    both("previews delete: running", "DELETE", P + "/7")
    queue_status = "SELECT status FROM application_deployment_queues WHERE application_id='<id>' AND pull_request_id=7"
    both("previews: deleted preview is gone", "GET", P + "/7")
    for side in SIDES:
        wait("preview row removed", lambda s=side: sql(fill(s, "SELECT count(*) FROM application_previews WHERE application_id=<pv_id> AND pull_request_id=7")) == "0")
    same_rows("previews: row removed", "SELECT count(*) FROM application_previews WHERE application_id=<pv_id> AND pull_request_id=7")
    for side in SIDES:
        names = vm(f"docker ps -a --filter label=coolify.applicationUuid={ctx[side]['pv']} --filter label=coolify.pullRequestId=7 --format '{{{{.Names}}}}'")
        record(f"previews: no container left ({side})", names == "", names)
    del queue_status
    both("previews delete: idle", "DELETE", P + "/5")
    for side in SIDES:
        wait("preview row removed", lambda s=side: sql(fill(s, "SELECT count(*) FROM application_previews WHERE application_id=<pv_id> AND pull_request_id=5")) == "0")
    both("previews store: again after deletion", "POST", P, {"pull_request_id": 5, "docker_tag": "1.27-alpine", "instant_deploy": False})
    same_rows("previews: row created again", PREVIEW_ROWS, *PREVIEW_DROP)
    # An application without a domain, and a port in the application's domain.
    N = "/applications/<nod>/previews"
    both("previews store: application without domain", "POST", N, {"pull_request_id": 1, "docker_tag": "x", "instant_deploy": False})
    same_rows("previews: row without domain", "SELECT * FROM application_previews WHERE application_id=<nod_id>", *PREVIEW_DROP)
    both("previews store: template with scheme", "POST", "/applications/<tpl>/previews", {"pull_request_id": 3, "docker_tag": "x", "instant_deploy": False})
    same_rows("previews: template row", "SELECT * FROM application_previews WHERE application_id=<tpl_id>", *PREVIEW_DROP)
    # A git application.
    G = "/applications/<gt>/previews"
    for name, body in [
        ("bad git type", {"pull_request_id": 1, "git_type": "svn"}),
        ("git type not string", {"pull_request_id": 1, "git_type": 1}),
        ("bad commit", {"pull_request_id": 1, "commit": "xyz"}),
        ("short commit", {"pull_request_id": 1, "commit": "abc"}),
        ("bitbucket without commit", {"pull_request_id": 1, "git_type": "bitbucket"}),
        ("bitbucket null commit", {"pull_request_id": 1, "git_type": "bitbucket", "commit": None}),
        ("tag on git", {"pull_request_id": 1, "docker_tag": "x"}),
    ]:
        both(f"previews store git: {name}", "POST", G, body)
    both("previews store git: source gives the type", "POST", G, {"pull_request_id": 1, "instant_deploy": False})
    both("previews store git: type overrides", "POST", G, {"pull_request_id": 2, "git_type": "gitlab", "instant_deploy": False})
    both("previews store git: bitbucket", "POST", G, {"pull_request_id": 3, "git_type": "bitbucket", "commit": "abcdef0", "instant_deploy": False})
    both("previews store git: existing keeps its type", "POST", G, {"pull_request_id": 2, "instant_deploy": False})
    same_rows("previews: git rows", "SELECT * FROM application_previews WHERE application_id=<gt_id> ORDER BY pull_request_id", *PREVIEW_DROP)
    both("previews: git list", "GET", G)
    # The queue row of a git preview: `git_type` and `commit` come through. Deleted right away, so the
    # reference's job does not build the pull request of a repository that has no such pull request.
    out = both("previews store git: queue with a commit", "POST", G, {"pull_request_id": 4, "git_type": "gitlab", "commit": "abcdef0123", "force": True})
    for side in SIDES:
        ctx[side]["dep"] = out[side].get("deployment_uuid")
    queue_rows("previews: git queue row", "application_id='<gt_id>' AND pull_request_id=4")
    both("previews delete: the git preview", "DELETE", G + "/4")
    for side in SIDES:
        wait("git preview removed", lambda s=side: sql(fill(s, "SELECT count(*) FROM application_previews WHERE application_id=<gt_id> AND pull_request_id=4")) == "0")
    # No source: the type is required.
    for side in SIDES:
        sql(fill(side, "UPDATE applications SET source_id=NULL, source_type=NULL WHERE id=<ns_id>"))
    both("previews store git: no source needs a type", "POST", "/applications/<ns>/previews", {"pull_request_id": 1})
    both("previews store git: no source with a type", "POST", "/applications/<ns>/previews", {"pull_request_id": 1, "git_type": "github", "instant_deploy": False})
    # An inline Dockerfile is not git based.
    both("previews store: inline dockerfile", "POST", "/applications/<df>/previews", {"pull_request_id": 1})
    compose_preview_cases()


def compose_preview_cases():
    C = "/applications/<cp>/previews"
    rows = "SELECT * FROM application_previews WHERE application_id=<cp_id> ORDER BY pull_request_id"
    both("previews compose: create", "POST", C, {"pull_request_id": 8, "git_type": "github", "instant_deploy": False})
    same_rows("previews compose: row", rows, *PREVIEW_DROP)
    same_rows("previews compose: volumes after create", "SELECT name, mount_path, host_path, is_preview_suffix_enabled FROM local_persistent_volumes "
              "WHERE resource_type='App\\Models\\Application' AND resource_id=<cp_id> ORDER BY name")
    # Known gap, not compared: upstream's parse with the pull request id leaves the rendered preview
    # file in `docker_compose`. That branch of the parser is not ported (KNOWN_LIMITATIONS.md).
    same_rows("previews compose: application row after create", "SELECT * FROM applications WHERE id=<cp_id>", "id", "uuid", "created_at", "updated_at", "last_online_at", "docker_compose",
              "manual_webhook_secret_github", "manual_webhook_secret_gitlab", "manual_webhook_secret_bitbucket", "manual_webhook_secret_gitea", "custom_labels")
    same_rows("previews compose: variables after create", "SELECT key, is_preview, is_buildtime, is_runtime FROM environment_variables "
              "WHERE resourceable_type='App\\Models\\Application' AND resourceable_id=<cp_id> ORDER BY key, is_preview")
    both("previews compose: show", "GET", C + "/8")
    P = C + "/8"
    for name, body in [
        ("no field", {}),
        ("domains field", {"domains": "https://x.example.com", "docker_compose_domains": []}),
        ("not array", {"docker_compose_domains": "x"}),
        ("bad item", {"docker_compose_domains": ["x"]}),
        ("extra key", {"docker_compose_domains": [{"name": "web", "foo": 1}]}),
        ("no name", {"docker_compose_domains": [{"domain": "https://a.example.com"}]}),
        ("name not string", {"docker_compose_domains": [{"name": 1}]}),
        ("duplicate names", {"docker_compose_domains": [{"name": "web"}, {"name": "web"}]}),
        ("bad domain", {"docker_compose_domains": [{"name": "web", "domain": "nope"}]}),
        ("bad redirect", {"docker_compose_domains": [{"name": "web", "redirect": "x"}]}),
        ("unknown service", {"docker_compose_domains": [{"name": "nope"}]}),
        ("database service", {"docker_compose_domains": [{"name": "my.db"}]}),
        ("duplicates", {"docker_compose_domains": [{"name": "web", "domain": "https://<cphost>"}, {"name": "api", "domain": "https://<cphost>:81"}]}),
    ]:
        both(f"previews compose patch: {name}", "PATCH", P, body)
    both("previews compose patch: domains and redirect", "PATCH", P, {"docker_compose_domains": [
        {"name": "web", "domain": "https://<cphost>:3000", "redirect": "www"}, {"name": "api", "domain": "https://api-<cphost>"}]})
    same_rows("previews compose: patched row", rows, *PREVIEW_DROP)
    both("previews compose patch: keeps redirect, clears other", "PATCH", P, {"docker_compose_domains": [{"name": "web", "domain": "https://<cphost>"}]})
    same_rows("previews compose: row after second patch", rows, *PREVIEW_DROP)
    both("previews compose patch: empty list", "PATCH", P, {"docker_compose_domains": []})
    same_rows("previews compose: row after empty", rows, *PREVIEW_DROP)
    both("previews compose: list", "GET", C)
    both("previews compose: delete", "DELETE", P)
    for side in SIDES:
        wait("compose preview removed", lambda s=side: sql(fill(s, "SELECT count(*) FROM application_previews WHERE application_id=<cp_id>")) == "0")
    same_rows("previews compose: volumes after delete", "SELECT name FROM local_persistent_volumes WHERE resource_type='App\\Models\\Application' AND resource_id=<cp_id> ORDER BY name")


# ---------------------------------------------------------------------------


def clone_rows(side, table, condition, drop):
    """Rows of one clone as JSON text, the new uuid and application id replaced, nothing else."""
    # A saved row moves in the table, and upstream copies them without an order.
    order = "mount_path" if table == "local_file_volumes" else "id"
    query = fill(side, f"SELECT coalesce(json_agg(row_to_json(x)), '[]')::text FROM (SELECT * FROM {table} WHERE {condition} ORDER BY {order}) x")
    items = json.loads(sql(query))
    out = []
    for item in items:
        for key in drop:
            item.pop(key, None)
        if item.get("custom_labels"):
            try:
                item["custom_labels"] = base64.b64decode(item["custom_labels"]).decode()
            except ValueError:
                pass
        out.append(item)
    text = json.dumps(out)
    return text.replace(ctx[side]["clone"], "NEWUUID")


def compare_clone(label, previews=True):
    """The rows both clones of one source have, in every table `clone_application()` writes."""
    common = ("id", "created_at", "updated_at")
    morph = "App\\Models\\Application"
    for table, condition, drop in [
        # `docker_compose`: the same known gap as for a Compose preview, see compose_preview_cases().
        ("applications", "id=<clone_id>", (*common, "last_online_at", "docker_compose") if label.startswith("compose") else (*common, "last_online_at")),
        ("application_settings", "application_id=<clone_id>", (*common, "application_id")),
        ("environment_variables", f"resourceable_type='{morph}' AND resourceable_id=<clone_id>", (*common, "resourceable_id")),
        ("local_persistent_volumes", f"resource_type='{morph}' AND resource_id=<clone_id>", (*common, "uuid", "resource_id")),
        ("local_file_volumes", f"resource_type='{morph}' AND resource_id=<clone_id>", (*common, "resource_id")),
        ("scheduled_tasks", "application_id=<clone_id>", (*common, "uuid", "application_id")),
        ("application_previews", "application_id=<clone_id>", (*common, "uuid", "application_id", "last_online_at") if previews else (*common,)),
        ("secret_manager_links", f"resourceable_type='{morph}' AND resourceable_id=<clone_id>", (*common, "uuid", "resourceable_id")),
    ]:
        a, b = clone_rows("ref", table, condition, drop), clone_rows("rs", table, condition, drop)
        record(f"clone {label}: {table}", a == b, row_differences(json.loads(a), json.loads(b)))
    same_rows(f"clone {label}: tags", "SELECT tag_id, taggable_type FROM taggables WHERE taggable_id=<clone_id> ORDER BY tag_id")
    for side in SIDES:
        status, raw = call(side, "GET", f"/api/v1/applications/{ctx[side]['clone']}", None, ROOT_TOK)
        if status != 200:
            record(f"clone {label}: readable ({side})", False, raw[:300])
    a = call("ref", "GET", f"/api/v1/applications/{ctx['rs']['clone']}", None, ROOT_TOK)
    record(f"clone {label}: the reference reads the rs clone", a[0] == 200, str(a)[:300])


def first_difference(a, b):
    at = next((i for i, (x, y) in enumerate(zip(a, b)) if x != y), min(len(a), len(b)))
    return f"at {at}\n  ref ..{a[max(0, at - 120):at + 240]}\n  rs  ..{b[max(0, at - 120):at + 240]}"


def clone_cases():
    both("clone: unknown app", "POST", "/applications/nope/clone", {"destination_uuid": PRIMARY_DEST})
    C = "/applications/<cl>/clone"
    for side in SIDES:
        ctx[side]["cl"] = ctx["ref"]["cl_src"]
    tiers("clone", "POST", C, {"destination_uuid": PRIMARY_DEST}, writes=False)
    both("clone: empty", "POST", C, {})
    both("clone: no json object", "POST", C, [])
    both("clone: no destination", "POST", C, {"name": "x"})
    both("clone: destination not string", "POST", C, {"destination_uuid": 1})
    both("clone: name too long", "POST", C, {"destination_uuid": PRIMARY_DEST, "name": "x" * 256})
    both("clone: name not string", "POST", C, {"destination_uuid": PRIMARY_DEST, "name": 5})
    both("clone: volumes not bool", "POST", C, {"destination_uuid": PRIMARY_DEST, "clone_volumes": "x"})
    both("clone: extra", "POST", C, {"destination_uuid": PRIMARY_DEST, "foo": 1})
    both("clone: unknown destination", "POST", C, {"destination_uuid": "nope"})
    both("clone: other team's destination", "POST", C, {"destination_uuid": PRIMARY_DEST}, OTHER)
    both_track("clone: with a write token, without volumes", "POST", C, {"destination_uuid": PRIMARY_DEST, "name": "opsdiff-write"}, WRITE, key="clone")
    both("clone: volumes with a write token", "POST", C, {"destination_uuid": PRIMARY_DEST, "clone_volumes": True}, WRITE)
    both("clone: volumes with a read-write token", "POST", C, {"destination_uuid": PRIMARY_DEST, "clone_volumes": True}, "acme_owner_read_write")

    def run(label, source, body):
        for side in SIDES:
            ctx[side]["cl"] = source
        both_track(f"clone {label}: response", "POST", C, body, key="clone")
        compare_clone(label)

    run("rich, default name, same server", ctx["ref"]["cl_src"], {"destination_uuid": PRIMARY_DEST})
    run("rich, name, other server", ctx["ref"]["cl_src"], {"destination_uuid": EDGE_DEST, "name": "opsdiff-named"})
    # The file storages keep the path under the source: nothing is written for the clone.
    for side in SIDES:
        record(f"clone: no directory for the clone ({side})", vm(f"ls -d /data/coolify/applications/{ctx[side]['clone']} 2>/dev/null") == "", "")
    run("rich, empty name", ctx["ref"]["cl_src"], {"destination_uuid": PRIMARY_DEST, "name": ""})
    extra_destinations(True)
    try:
        run("swarm destination", ctx["ref"]["cl_src"], {"destination_uuid": "<swarm_dest>", "name": "opsdiff-swarm"})
    finally:
        # The clones go first: a clone cannot be deleted through the API without its destination.
        clones = [ctx[side]["clone"] for side in SIDES]
        for uuid in clones:
            call("ref", "DELETE", f"/api/v1/applications/{uuid}?delete_volumes=false&delete_connected_networks=false&delete_configurations=false", None, ROOT_TOK)
        wait("swarm clones deleted", lambda: sql("SELECT count(*) FROM applications WHERE uuid IN ('%s','%s')" % tuple(clones)) == "0", 120)
        extra_destinations(False)
    run("git, null columns", ctx["ref"]["cl_git"], {"destination_uuid": PRIMARY_DEST})
    run("compose with a preview", ctx["ref"]["cl_compose"], {"destination_uuid": PRIMARY_DEST})
    for side in SIDES:
        ctx[side]["cl"] = ctx["ref"]["cl_collide"]
    both("clone: a preview's volume name comes out twice (500 on both)", "POST", C, {"destination_uuid": PRIMARY_DEST})
    run("labels not read-only", ctx["ref"]["cl_plain"], {"destination_uuid": PRIMARY_DEST, "name": "opsdiff-plain"})


def clone_volume_cases():
    C = "/applications/<cv>/clone"
    out = both("clone volumes: response", "POST", C, {"destination_uuid": PRIMARY_DEST, "name": "opsdiff-cv", "clone_volumes": True})
    for side in SIDES:
        uuid = out[side]["uuid"]
        track(uuid, side)
        ctx[side]["clone"] = uuid
        ctx[side]["clone_id"] = sql(f"SELECT id FROM applications WHERE uuid='{uuid}'")
        name = sql(f"SELECT name FROM local_persistent_volumes WHERE resource_type='App\\Models\\Application' AND resource_id={ctx[side]['clone_id']}")
        state["volumes"].append(name)
        save()
        copied = wait("volume copy", lambda n=name: vm(f"docker run --rm -v {n}:/d alpine cat /d/marker 2>/dev/null") == "copied-marker", 180)
        record(f"clone volumes: data copied ({side})", copied, vm(f"docker volume inspect {name}")[:200])
        wait("source redeployment", lambda s=side: sql(fill(s, "SELECT count(*) FROM application_deployment_queues WHERE application_id='<cv_id>' AND status IN ('finished','failed')")) != "0")
        wait("source redeployment", lambda s=side: sql(fill(s, "SELECT count(*) FROM application_deployment_queues WHERE application_id='<cv_id>' AND status IN ('queued','in_progress')")) == "0")
    same_rows("clone volumes: volume row flag", "SELECT ignores_compose_driver_options, mount_path FROM local_persistent_volumes "
              "WHERE resource_type='App\\Models\\Application' AND resource_id=<clone_id>")
    same_rows("clone volumes: the source's queue row", "SELECT * FROM application_deployment_queues WHERE application_id='<cv_id>' ORDER BY id", *QUEUE_DROP, "deployment_url")
    same_rows("clone volumes: the deployment's end state", "SELECT status FROM application_deployment_queues WHERE application_id='<cv_id>' ORDER BY id")


# ---------------------------------------------------------------------------


def rollback_cases():
    R = "/applications/<rb>/rollback"
    both("rollback: unknown app", "POST", "/applications/nope/rollback", {"commit": "x"})
    for label, tok in [("read token", READ), ("write token", WRITE), ("member legacy", MEMBER_WRITE), ("other team", OTHER)]:
        both(f"rollback: {label}", "POST", R, {"commit": "1.26-alpine"}, tok)
    for name, body in [
        ("empty", {}),
        ("no commit", {"foo": 1}),
        ("not string", {"commit": 5}),
        ("extra", {"commit": "1.26-alpine", "foo": 1}),
        ("hyphen", {"commit": "-x"}),
        ("disallowed", {"commit": "a b;c"}),
        ("first character", {"commit": ".x"}),
    ]:
        both(f"rollback: {name}", "POST", R, body)
    both("rollback images: unknown app", "GET", "/applications/nope/rollback-images")
    both("rollback images: other team", "GET", "/applications/<rb>/rollback-images", tok=OTHER)
    both("rollback images: never deployed", "GET", "/applications/<rb>/rollback-images", status_only=True)
    out = both("rollback: queued", "POST", R, {"commit": "1.26-alpine"})
    for side in SIDES:
        ctx[side]["dep"] = out[side].get("deployment_uuid")
    queue_rows("rollback: queue row", "application_id='<rb_id>'")
    end = {side: wait_deployment(ctx[side]["dep"]) for side in SIDES}
    record("rollback: both finished", end["ref"] == end["rs"] == "finished", str(end))
    images = {side: vm(f"docker ps --filter label=coolify.applicationUuid={ctx[side]['rb']} --format '{{{{.Image}}}}'") for side in SIDES}
    record("rollback: the container runs the commit as its tag", images["ref"] == images["rs"] == "nginx:1.26-alpine", str(images))
    queue_rows("rollback: queue row after the job", "application_id='<rb_id>'")
    cross_read("rollback images: running application", "/applications/<rb>/rollback-images")
    cross_read("rollback images: read token", "/applications/<rb>/rollback-images", READ)
    cross_read("rollback images: member", "/applications/<rb>/rollback-images", MEMBER_READ)
    # Registry images: commits of finished deployments of a git application with an image name.
    I = "/applications/<reg>/rollback-images"
    for side in SIDES:
        for n, commit in enumerate(["aaaaaaaaaaaa", "bbbbbbbbbbbb", "aaaaaaaaaaaa", "HEAD"]):
            status = "finished"
            uuid = f"opsdiff-{RUN}-{side}-{n}"
            state["queue"].append(uuid)
            save()
            sql(fill(side, f"INSERT INTO application_deployment_queues (application_id, deployment_uuid, pull_request_id, commit, status, created_at, updated_at) "
                           f"VALUES ('<reg_id>', '{uuid}', 0, '{commit}', '{status}', '2026-01-0{n + 1} 00:00:00', '2026-01-0{n + 1} 00:00:00')"))
        uuid = f"opsdiff-{RUN}-{side}-pr"
        state["queue"].append(uuid)
        save()
        sql(fill(side, f"INSERT INTO application_deployment_queues (application_id, deployment_uuid, pull_request_id, commit, status, created_at, updated_at) "
                       f"VALUES ('<reg_id>', '{uuid}', 3, 'cccccccccccc', 'finished', now(), now())"))
    cross_read("rollback images: registry commits", I)


def log_cases():
    """One running application, read through both."""
    L = fill("ref", "/api/v1/applications/<lg>/logs")
    uuid = ctx["ref"]["lg"]
    status, raw = call("ref", "POST", f"/api/v1/applications/{uuid}/start", None, ROOT_TOK)
    dep = json.loads(raw).get("deployment_uuid")
    end = wait_deployment(dep)
    record("logs: the application is deployed by the reference", end == "finished", end)
    name = vm(f"docker ps --filter label=coolify.applicationUuid={uuid} --format '{{{{.Names}}}}'")
    secret = "opsdiffsecretvalue42"
    line = f"plain line\\n\\x1b[31mred line\\x1b[0m\\nvalue {secret}\\npreview-only opsdiffpreviewvalue\\nAPI_KEY=abcdef123456\\n"
    vm(f"docker exec {name} sh -c \"printf '{line}' > /proc/1/fd/1\"")
    time.sleep(1)

    def read(label, query="", tok=ROOT_TOK, path=L):
        a, b = call("ref", "GET", path + query, None, tok), call("rs", "GET", path + query, None, tok)
        record(f"logs: {label}", a == b, first_difference(str(a), str(b)))
        return a

    got = read("default")
    record("logs: the secret is redacted", secret.encode() not in got[1] and b"plain line" in got[1], got[1][-500:])
    read("two lines", "?lines=2")
    read("zero lines", "?lines=0")
    read("all", "?lines=all")
    read("minus one", "?lines=-1")
    read("minus two", "?lines=-2")
    read("not a number", "?lines=abc")
    read("leading zero", "?lines=05")
    read("over the maximum", "?lines=20000")
    read("empty lines", "?lines=")
    read("timestamps", "?lines=3&show_timestamps=true")
    read("timestamps yes", "?lines=3&show_timestamps=yes")
    read("timestamps off", "?lines=3&show_timestamps=false")
    read("timestamps invalid", "?lines=3&show_timestamps=maybe")
    read("service not running", "?service_name=nope")
    read("empty service", "?service_name=&lines=1")
    read("read token", "", READ)
    read("write token", "", WRITE)
    read("member read token", "", MEMBER_READ)
    read("member write token", "", MEMBER_WRITE)
    read("other team", "", OTHER)
    read("unknown app", "", ROOT_TOK, "/api/v1/applications/nope/logs")
    both("logs: not running", "GET", "/applications/<nod>/logs")
    cross_read("rollback images: application with its image name", "/applications/<lg>/rollback-images")
    stopped = call("ref", "POST", f"/api/v1/applications/{uuid}/stop", None, ROOT_TOK)
    print("  stop:", stopped[0])
    wait("container stopped", lambda: vm(f"docker ps --filter label=coolify.applicationUuid={uuid} --format '{{{{.Names}}}}'") == "", 120)
    read("no container", "")


# ---------------------------------------------------------------------------


def setup_shared():
    project = state["project"]
    envs = json.loads(call("ref", "GET", f"/api/v1/projects/{project}", None, ROOT_TOK)[1])["environments"]
    prod = next(e["uuid"] for e in envs if e["name"] == "production")
    status, raw = call("ref", "POST", f"/api/v1/projects/{project}/environments", {"name": "opsdiff-target"}, ROOT_TOK)
    target = json.loads(raw)["uuid"]
    for side in SIDES:
        ctx[side].update(env_prod=prod, env_target=target, project=project)
    for side in SIDES:
        ctx[side].update(pvhost2=f"opsdiff-{RUN}-{side}-a.example.com", cphost=f"opsdiff-{RUN}-{side}-c.example.com")
        ctx[side]["shared_dom"] = f"opsdiff-{RUN}-{side}-shared.example.com"
    return project


def extra_destinations(present):
    """A second destination on a server makes creating applications there without `destination_uuid`
    fail, and the database is shared: these rows exist only on edge-1 and only during the checks that
    need them, after this run's applications exist."""
    for key, table, column in [("edge_second_dest", "standalone_dockers", "docker"), ("swarm_dest", "swarm_dockers", "swarm")]:
        uuid = f"opsdiff{RUN}{key.replace('_', '')}"
        if present:
            state[column].append(uuid)
            save()
            sql(f"INSERT INTO {table} (name, uuid, network, server_id, created_at, updated_at) VALUES ('opsdiff', '{uuid}', 'opsdiff-{key}', 21, now(), now())")
            for side in SIDES:
                ctx[side][key] = uuid
        else:
            sql(f"DELETE FROM {table} WHERE uuid='{uuid}'")


def setup_clone_sources(base):
    git = dict(base, git_repository=REPO, git_branch="main", ports_exposes="3000", instant_deploy=False)
    image = dict(base, docker_registry_image_name="nginx", docker_registry_image_tag="1.27-alpine", ports_exposes="80", instant_deploy=False)
    src = make("ref", "dockerimage", dict(image, name="opsdiff-a-very-long-source-application-name", domains=f"https://opsdiff-{RUN}-src.example.com:8080,https://b-opsdiff-{RUN}-src.example.com",
                                          force_domain_override=True, description="rich", is_auto_deploy_enabled=False, health_check_enabled=True))
    A = f"/api/v1/applications/{src}"

    def ref(method, path, body=None):
        status, raw = call("ref", method, A + path, body, ROOT_TOK)
        if status >= 300:
            raise SystemExit(f"setup {path}: {status} {raw[:400]}")
        return parsed(raw)

    ref("POST", "/envs", {"key": "PLAIN", "value": "plain-value"})
    ref("POST", "/envs", {"key": "BUILD_ONLY", "value": "b", "is_buildtime": True, "is_runtime": False})
    ref("POST", "/envs", {"key": "ONLY_ONCE", "value": "once-value", "is_shown_once": True})
    ref("POST", "/envs", {"key": "Z_REQUIRED", "value": "v", "is_required": True} if False else {"key": "Z_LAST", "value": "v"})
    ref("POST", "/envs", {"key": "A_FIRST", "value": "v", "is_multiline": True, "comment": "c"})
    ref("POST", "/envs", {"key": "PREVIEW_ONLY", "value": "pv", "is_preview": True})
    ref("POST", "/storages", {"type": "persistent", "name": f"{src}-data", "mount_path": "/data"})
    ref("POST", "/storages", {"type": "persistent", "name": "custom-name-last", "mount_path": "/other"})
    ref("POST", "/storages", {"type": "persistent", "name": "nodash", "mount_path": "/third"})
    ref("POST", "/storages", {"type": "file", "mount_path": "/etc/opsdiff.conf", "content": "file-content"})
    ref("POST", "/storages", {"type": "file", "mount_path": "/etc/opsdiff.d", "is_directory": True, "fs_path": f"/data/coolify/applications/{src}/opsdiff.d"})
    tag = f"opsdiff-tag-{RUN}"
    state["tags"].append(tag)
    save()
    ref("POST", "/tags", {"tag_name": tag})
    ref("POST", "/scheduled-tasks", {"name": "z-task", "command": "echo z", "frequency": "daily"})
    ref("POST", "/scheduled-tasks", {"name": "a-task", "command": "echo a", "frequency": "hourly", "container": "web", "timeout": 60, "enabled": False})
    ref("POST", "/previews", {"pull_request_id": 11, "docker_tag": "1.27-alpine", "pull_request_html_url": "https://example.com/pull/11", "instant_deploy": False})
    ref("POST", "/previews", {"pull_request_id": 12, "docker_tag": "1.26-alpine", "instant_deploy": False})
    token_id = tinker(r"""
$t = \App\Models\IntegrationToken::create(["name" => "opsdiff-%s", "provider" => "doppler", "team_id" => %d, "capabilities" => ["secrets"], "token" => "dp.st.fake", "metadata" => []]);
echo json_encode([$t->uuid, $t->id]);
""" % (RUN, int(sub_tokens()["acme_team_id"])))
    match = re.search(r'\["([^"]+)",(\d+)\]', token_id)
    if not match:
        raise SystemExit(f"integration token: {token_id}")
    state["tokens"].append(int(match.group(2)))
    save()
    ref("PATCH", "/secret-manager", {"integration_token_uuid": match.group(1)})
    # Settings the copy carries over.
    sid = sql(f"SELECT id FROM applications WHERE uuid='{src}'")
    sql(f"UPDATE application_settings SET is_debug_enabled=true, is_gzip_enabled=false WHERE application_id={sid}")
    sql(f"UPDATE applications SET noindex_domains='[\"https://opsdiff-{RUN}-src.example.com\"]'::json WHERE id={sid}")
    ctx["ref"]["cl_src"] = src

    git_src = make("ref", "public", dict(git, name="opsdiff-git", build_pack="nixpacks", domains=f"https://opsdiff-{RUN}-git.example.com"))
    gid = sql(f"SELECT id FROM applications WHERE uuid='{git_src}'")
    sql(f"UPDATE applications SET install_command='  npm ci  ', build_command=NULL WHERE id={gid}")
    ctx["ref"]["cl_git"] = git_src

    compose_src = make("ref", "public", dict(base, name="opsdiff-cl-compose", git_repository=REPO, git_branch="main", build_pack="dockercompose",
                                             ports_exposes="3000", docker_compose_location="/missing/docker-compose.yml", instant_deploy=False))
    cid = sql(f"SELECT id FROM applications WHERE uuid='{compose_src}'")
    # No domains: a generated preview domain would be the same for every clone and hit the unique
    # index on the second one, on both implementations.
    set_compose(cid, None, COMPOSE_NO_VOLUME)
    status, raw = call("ref", "POST", f"/api/v1/applications/{compose_src}/previews", {"pull_request_id": 21, "git_type": "github", "instant_deploy": False}, ROOT_TOK)
    if status != 201:
        raise SystemExit(f"compose clone preview: {status} {raw[:300]}")
    ctx["ref"]["cl_compose"] = compose_src
    # With a named volume: the preview's volume name comes out twice in `clone_application()`.
    collision = make("ref", "public", dict(base, name="opsdiff-cl-collide", git_repository=REPO, git_branch="main", build_pack="dockercompose",
                                           ports_exposes="3000", docker_compose_location="/missing/docker-compose.yml", instant_deploy=False))
    set_compose(sql(f"SELECT id FROM applications WHERE uuid='{collision}'"), None)
    status, raw = call("ref", "POST", f"/api/v1/applications/{collision}/previews", {"pull_request_id": 22, "git_type": "github", "instant_deploy": False}, ROOT_TOK)
    if status != 201:
        raise SystemExit(f"compose clone preview: {status} {raw[:300]}")
    ctx["ref"]["cl_collide"] = collision

    plain = make("ref", "dockerimage", dict(image, name="opsdiff-plain-source", domains=f"https://opsdiff-{RUN}-plain.example.com", force_domain_override=True))
    pid = sql(f"SELECT id FROM applications WHERE uuid='{plain}'")
    sql(f"UPDATE application_settings SET is_container_label_readonly_enabled=false WHERE application_id={pid}")
    ctx["ref"]["cl_plain"] = plain


def set_compose(app_id, host, compose=COMPOSE):
    compose = compose.replace("'", "''")
    domains = json.dumps({"web": {"domain": f"https://{host}:3000" if host else ""}, "api": {"domain": ""}})
    sql(f"UPDATE applications SET docker_compose_raw='{compose}', docker_compose='{compose}', docker_compose_domains='{domains}' WHERE id={app_id}")


def sub_tokens():
    return json.load(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "../reference/secrets/tokens.json")))


def setup_side_apps(base):
    image = dict(base, docker_registry_image_name="nginx", docker_registry_image_tag="1.27-alpine", ports_exposes="80", instant_deploy=False)
    git = dict(base, git_repository=REPO, git_branch="main", ports_exposes="3000", instant_deploy=False, build_pack="nixpacks")
    each_side("mv", "dockerimage", dict(image, name="opsdiff-mv"))
    each_side("ds", "dockerimage", dict(image, name="opsdiff-ds"))
    each_side("eg", "dockerimage", dict(image, name="opsdiff-eg", server_uuid="forphxqhvwwipnx1enmed1t7", destination_uuid=EDGE_DEST))
    each_side("ps", "dockerimage", dict(image, name="opsdiff-ps"))
    for side in SIDES:
        call(side, "POST", fill(side, "/api/v1/applications/<ps>/storages"), {"type": "persistent", "name": "opsdiff-ps-vol", "mount_path": "/data"}, ROOT_TOK)
    for side in SIDES:
        ctx[side]["pvhost"] = f"opsdiff-{RUN}-{side}-pv.example.com"
        ctx[side]["tplhost"] = f"opsdiff-{RUN}-{side}-tpl.example.com"
        ctx[side]["cphost0"] = f"opsdiff-{RUN}-{side}-c0.example.com"
    each_side("pv", "dockerimage", dict(image, name="opsdiff-pv", domains="https://<pvhost>:8080,https://other-<pvhost>", force_domain_override=True))
    each_side("nod", "dockerimage", dict(image, name="opsdiff-nod"))
    for side in SIDES:
        sql(fill(side, "UPDATE applications SET fqdn=NULL WHERE id=<nod_id>"))
    each_side("tpl", "dockerimage", dict(image, name="opsdiff-tpl", domains="https://<tplhost>/app", force_domain_override=True))
    for side in SIDES:
        sql(fill(side, "UPDATE applications SET preview_url_template='http://pr-{{pr_id}}.{{domain}}' WHERE id=<tpl_id>"))
    each_side("gt", "public", dict(git, name="opsdiff-gt"))
    each_side("ns", "public", dict(git, name="opsdiff-ns"))
    dockerfile = base64.b64encode(b"FROM nginx:alpine\n").decode()
    each_side("df", "dockerfile", dict(base, name="opsdiff-df", dockerfile=dockerfile, ports_exposes="80", instant_deploy=False))
    each_side("cp", "public", dict(base, name="opsdiff-cp", git_repository=REPO, git_branch="main", build_pack="dockercompose",
                                   ports_exposes="3000", docker_compose_location="/missing/docker-compose.yml", instant_deploy=False))
    for side in SIDES:
        set_compose(ctx[side]["cp_id"], ctx[side]["cphost0"])
    # A domain another application of the team uses.
    each_side("shared", "dockerimage", dict(image, name="opsdiff-shared", domains="https://<shared_dom>", force_domain_override=True))
    each_side("rb", "dockerimage", dict(image, name="opsdiff-rb"))
    uuid = make("ref", "public", dict(git, name="opsdiff-reg", docker_registry_image_name="ghcr.io/opsdiff/app"))
    for side in SIDES:
        ctx[side]["reg"] = uuid
        ctx[side]["reg_id"] = sql(f"SELECT id FROM applications WHERE uuid='{uuid}'")
    each_side("cv", "dockerimage", dict(image, name="opsdiff-cv-source"))
    for side in SIDES:
        name = f"opsdiff-{RUN}-{side}-vol"
        state["volumes"].append(name)
        save()
        status, raw = call(side, "POST", fill(side, "/api/v1/applications/<cv>/storages"), {"type": "persistent", "name": "vol", "mount_path": "/data"}, ROOT_TOK)
        if status >= 300:
            raise SystemExit(f"volume: {status} {raw[:300]}")
        state["volumes"].append(f"{ctx[side]['cv']}-vol")
        save()
        vm(f"docker volume create {ctx[side]['cv']}-vol >/dev/null; docker run --rm -v {ctx[side]['cv']}-vol:/d alpine sh -c 'printf copied-marker > /d/marker'")
    secret_env = {"key": "OPSDIFF_SECRET", "value": "opsdiffsecretvalue42"}
    lg = make("ref", "dockerimage", dict(image, name="opsdiff-lg"))
    call("ref", "POST", f"/api/v1/applications/{lg}/envs", secret_env, ROOT_TOK)
    call("ref", "POST", f"/api/v1/applications/{lg}/envs", {"key": "OPSDIFF_PV", "value": "opsdiffpreviewvalue", "is_preview": True}, ROOT_TOK)
    for side in SIDES:
        ctx[side]["lg"] = lg


def cleanup(st):
    """Deletes what this run created, by the uuids and ids in its state file. The project is this
    run's, so everything in it is too (a clone that failed half way returns no uuid)."""
    if st["project"]:
        rows = sql(f"SELECT a.uuid, a.id FROM applications a JOIN environments e ON e.id=a.environment_id JOIN projects p ON p.id=e.project_id WHERE p.uuid='{st['project']}'")
        known = {a["uuid"] for a in st["apps"]}
        for line in rows.splitlines():
            uuid, app_id = line.split("|")
            if uuid not in known:
                st["apps"].append({"side": "ref", "uuid": uuid, "id": app_id})
    for app in st["apps"]:
        call("ref", "DELETE", f"/api/v1/applications/{app['uuid']}?delete_volumes=true&delete_connected_networks=true&delete_configurations=true", None, ROOT_TOK)
    uuids = ",".join(f"'{a['uuid']}'" for a in st["apps"]) or "''"
    for _ in range(100):
        if sql(f"SELECT count(*) FROM applications WHERE uuid IN ({uuids})") == "0":
            break
        time.sleep(3)
    left = sql(f"SELECT uuid FROM applications WHERE uuid IN ({uuids})")
    if left:
        print("not deleted through the API:\n" + left)
    ids = ",".join(str(a["id"]) for a in st["apps"]) or "0"
    id_text = ",".join(f"'{a['id']}'" for a in st["apps"]) or "''"
    # Soft deleted previews stay behind the application's force delete.
    sql(f"DELETE FROM application_previews WHERE application_id IN ({ids})")
    sql(f"DELETE FROM additional_destinations WHERE application_id IN ({ids})")
    sql(f"DELETE FROM application_deployment_queues WHERE application_id IN ({id_text})")
    for uuid in st["queue"]:
        sql(f"DELETE FROM application_deployment_queues WHERE deployment_uuid='{uuid}'")
    for name in st["tags"]:
        sql(f"DELETE FROM tags WHERE name='{name}'")
    for token in st["tokens"]:
        sql(f"DELETE FROM integration_tokens WHERE id={token}")
    for uuid in st["docker"]:
        sql(f"DELETE FROM standalone_dockers WHERE uuid='{uuid}'")
    for uuid in st["swarm"]:
        sql(f"DELETE FROM swarm_dockers WHERE uuid='{uuid}'")
    for name in st["volumes"]:
        vm(f"docker volume rm -f {name} >/dev/null 2>&1")
    for app in st["apps"]:
        vm(f"docker ps -a --filter label=coolify.applicationUuid={app['uuid']} -q | xargs -r docker rm -f; rm -rf /data/coolify/applications/{app['uuid']}")
    if st["project"]:
        call("ref", "DELETE", f"/api/v1/projects/{st['project']}", None, ROOT_TOK)
        for env in json.loads(call("ref", "GET", f"/api/v1/projects/{st['project']}", None, ROOT_TOK)[1] or b"{}").get("environments", []):
            call("ref", "DELETE", f"/api/v1/projects/{st['project']}/environments/{env['uuid']}", None, ROOT_TOK)
        call("ref", "DELETE", f"/api/v1/projects/{st['project']}", None, ROOT_TOK)


def main():
    global args
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", default="hml20fsj5eles0xfiqidq8j5")
    ap.add_argument("--vm", default="coolify-rs-e2e")
    ap.add_argument("--state", required=True)
    ap.add_argument("--cleanup", action="store_true", help="only delete what the state file lists")
    ap.add_argument("--only", default="", help="comma separated: move,migrate,destinations,previews,clone,volumes,rollback,logs")
    args = ap.parse_args()
    if args.cleanup:
        cleanup(json.load(open(args.state)))
        return
    only = set(filter(None, args.only.split(",")))
    want = lambda k: not only or k in only  # noqa: E731
    project = json.loads(call("ref", "POST", "/api/v1/projects", {"name": f"opsdiff-{RUN}"}, ROOT_TOK)[1])["uuid"]
    state["project"] = project
    save()
    base = {"server_uuid": args.server, "project_uuid": project, "environment_name": "production"}
    try:
        setup_shared()
        setup_side_apps(base)
        setup_clone_sources(base)
        steps = [("move", move_cases), ("migrate", migrate_cases), ("destinations", destination_cases), ("previews", preview_cases),
                 ("clone", clone_cases), ("volumes", clone_volume_cases), ("rollback", rollback_cases), ("logs", log_cases)]
        for key, step in steps:
            if want(key):
                print(f"== {key}", flush=True)
                step()
    finally:
        cleanup(state)
    passed = sum(1 for r in results if r)
    print(f"{passed}/{len(results)} checks passed")
    sys.exit(0 if passed == len(results) else 1)


if __name__ == "__main__":
    main()
