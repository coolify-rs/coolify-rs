#!/usr/bin/env python3
"""Differential check of scheduled tasks: the API, `ScheduledTaskJob` on the test VM, and the scheduler.

API checks: each side has its own stopped application and every request goes to both, with several
tokens. Execution checks: one application the reference deployed, and for each command one task per side
made through that side's API with `enabled: false`, so no scheduler fires it. `POST .../execute` runs it,
and the execution rows and the command line on the VM are compared.

With `--scheduler` (start the rs side with COOLIFY_RS_SCHEDULER=on for this run, on a database nobody else uses: the
scheduler now also runs the server checks and Docker cleanups of every row; compat/diff/scheduler_window.py is the safe
run on the shared VM) one enabled task with a
`* * * * *` frequency exists for a few minutes on the running application, with both schedulers on: each
minute must have exactly one execution. A task with a far away frequency stays unfired.

Everything created is tracked by uuid and id in the state file and deleted at the end; `--cleanup <file>`
finishes a run that crashed. Usage:

    COOLIFY_RS_BASE=http://127.0.0.1:18082 applications_scheduled_tasks_diff.py \
        --server hml20fsj5eles0xfiqidq8j5 --vm coolify-rs-e2e --state /tmp/tasks-state.json [--scheduler]

Exit status 1 when anything differs.
"""
import argparse
import hashlib
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import applications_subresources_diff as sub  # noqa: E402
from applications_subresources_diff import (MEMBER_READ, MEMBER_WRITE, OTHER, READ, ROOT_TOK, SIDES, WRITE, both, call, ctx,  # noqa: E402
                                            fill, parsed, tinker)
from databases_diff import BASES, TOKENS, record, results, sql  # noqa: E402

RUN = str(int(time.time()))
FAR = "0 0 1 1 *"
TASK_DROP = ("id", "uuid", "created_at", "updated_at", "application_id")
EXEC_DROP = ("id", "uuid", "scheduled_task_id", "created_at", "updated_at", "finished_at", "started_at", "duration")
TASKS = "/applications/<app>/scheduled-tasks"
state = {"apps": [], "project": None, "tasks": [], "containers": []}
args = None
sub.UUIDISH |= {"task_uuid"}


def save():
    with open(args.state, "w") as f:
        json.dump(state, f)


def vm(cmd):
    return subprocess.run(["orb", "-m", args.vm, "-u", "root", "bash", "-c", cmd], capture_output=True, text=True).stdout.strip()


def raw(side, method, path, data, ctype):
    r = urllib.request.Request(BASES[side] + "/api/v1" + fill(side, path), data=data, method=method)
    r.add_header("Authorization", "Bearer " + TOKENS[ROOT_TOK])
    r.add_header("Accept", "application/json")
    if ctype:
        r.add_header("Content-Type", ctype)
    for _ in range(40):
        try:
            resp = urllib.request.urlopen(r)
            return resp.status, resp.read()
        except urllib.error.HTTPError as e:
            if e.code == 429:
                time.sleep(int(e.headers.get("Retry-After", "5")))
                continue
            return e.code, e.read()
    raise SystemExit("rate limited")


def both_raw(name, method, path, data, ctype):
    a, b = raw("ref", method, path, data, ctype), raw("rs", method, path, data, ctype)
    record(name, a == b, f"ref {a}\n  rs  {b}")


def track_task(uuid):
    task_id = sql(f"SELECT id FROM scheduled_tasks WHERE uuid='{uuid}'")
    if task_id:
        state["tasks"].append({"uuid": uuid, "id": task_id})
        save()
    return task_id


def track_new(side_results):
    for side in SIDES:
        data = side_results[side]
        if isinstance(data, dict) and data.get("uuid"):
            track_task(data["uuid"])


def create_both(name, body, tok=ROOT_TOK, key=None):
    """A create on both sides; a side's new task is tracked and named `<key>`."""
    out = both(name, "POST", TASKS, body, tok)
    track_new(out)
    if key:
        for side in SIDES:
            ctx[side][key] = out[side].get("uuid", "missing") if isinstance(out[side], dict) else "missing"
    return out


def tiers(prefix, method, path, body=None, tokens=(READ, WRITE, MEMBER_READ, MEMBER_WRITE, OTHER)):
    labels = {READ: "read token", WRITE: "write token", MEMBER_READ: "member read", MEMBER_WRITE: "member legacy", OTHER: "other team"}
    for tok in tokens:
        both(f"{prefix}: {labels[tok]}", method, path, body, tok)


def task_rows(name):
    sub.same_rows(name, "SELECT * FROM scheduled_tasks WHERE application_id=<app_id> ORDER BY name, frequency", *TASK_DROP)


# ---------------------------------------------------------------------------
# API


def list_cases():
    both("list: empty", "GET", TASKS)
    tiers("list: empty", "GET", TASKS)
    both("list: unknown application", "GET", "/applications/nope/scheduled-tasks")


def create_validation():
    good = {"name": "n", "command": "true", "frequency": FAR}
    both_raw("create: no content type", "POST", TASKS, b'{"name":"x"}', None)
    both_raw("create: form content type", "POST", TASKS, b"name=x", "application/x-www-form-urlencoded")
    both_raw("create: invalid JSON", "POST", TASKS, b"{nope", "application/json")
    both_raw("create: empty object", "POST", TASKS, b"{}", "application/json")
    both_raw("create: empty array", "POST", TASKS, b"[]", "application/json")
    both_raw("create: a list", "POST", TASKS, b'[{"name":"x"}]', "application/json")
    both_raw("create: a number", "POST", TASKS, b"5", "application/json")
    both_raw("create: zero", "POST", TASKS, b"0", "application/json")
    both_raw("create: null", "POST", TASKS, b"null", "application/json")
    both("create: nothing required", "POST", TASKS, {"timeout": 5})
    both("create: only name", "POST", TASKS, {"name": "x"})
    both("create: extra field", "POST", TASKS, dict(good, id=5, nope="x"))
    both("create: extra field and missing", "POST", TASKS, {"nope": "x"})
    for key, values in {
        "name": [None, "", "   ", 5, True, [], {}, "x" * 256],
        "command": [None, "", 5, False, ["a"]],
        "frequency": [None, "", 5, [], "garbage", "* * *", "* * * * * *", "61 * * * *", "0 0 31 2 *", "*/0 * * * *", "0", "@reboot", "@annually", "@midnight"],
        "container": [5, True, [], {}],
        "timeout": [None, "", 0, -1, 36001, "abc", 1.5, True, [], "1.5"],
        "enabled": [None, "", "maybe", 5, [], "yes", "on", "true", 2],
    }.items():
        for value in values:
            both(f"create: {key}={json.dumps(value)[:30]}", "POST", TASKS, dict(good, **{key: value}))
    track_tasks_of_app()


def track_tasks_of_app():
    """Some invalid looking values may be accepted; such a task is tracked like any other."""
    for side in SIDES:
        known = {t["uuid"] for t in state["tasks"]}
        for line in sql(fill(side, "SELECT uuid FROM scheduled_tasks WHERE application_id=<app_id>")).splitlines():
            if line not in known:
                track_task(line)


def clear_tasks():
    """Deletes the tasks of the side applications through each side's API."""
    for side in SIDES:
        for line in sql(fill(side, "SELECT uuid FROM scheduled_tasks WHERE application_id=<app_id>")).splitlines():
            call(side, "DELETE", fill(side, f"/api/v1/applications/<app>/scheduled-tasks/{line}"), None, ROOT_TOK)


def create_cases():
    tasks = {"name": "tiers", "command": "true", "frequency": FAR}
    tiers("create: enabled by default", "POST", TASKS, tasks, (READ, WRITE, MEMBER_READ, MEMBER_WRITE, OTHER))
    tiers("create: enabled true", "POST", TASKS, dict(tasks, enabled=True), (READ, WRITE, MEMBER_WRITE))
    create_both("create: write token, disabled", dict(tasks, name="write-disabled", enabled=False), WRITE)
    create_both("create: write token, disabled as 0", dict(tasks, name="write-disabled-0", enabled=0), WRITE)
    create_both("create: write token, disabled as string", dict(tasks, name="write-disabled-str", enabled="0"), WRITE)
    both("create: member legacy, disabled", "POST", TASKS, dict(tasks, name="m", enabled=False), MEMBER_WRITE)
    both("create: unknown application", "POST", "/applications/nope/scheduled-tasks", tasks)

    create_both("create: defaults", {"name": "z-default", "command": "echo hi", "frequency": FAR}, key="t1")
    create_both("create: every field", {"name": "a-every", "command": "echo 'x' \"y\"", "frequency": "*/5 1-3 1,15 jan mon", "container": "web", "timeout": 20,
                                        "enabled": False}, key="t2")
    create_both("create: timeout as string", {"name": "timeout-str", "command": "true", "frequency": FAR, "timeout": "45", "enabled": False})
    create_both("create: timeout at the bounds", {"name": "timeout-max", "command": "true", "frequency": FAR, "timeout": 36000, "enabled": False})
    create_both("create: timeout one", {"name": "timeout-one", "command": "true", "frequency": FAR, "timeout": 1, "enabled": False})
    create_both("create: null container", {"name": "container-null", "command": "true", "frequency": FAR, "container": None, "enabled": False})
    create_both("create: empty container", {"name": "container-empty", "command": "true", "frequency": FAR, "container": "", "enabled": False})
    create_both("create: html in the name", {"name": "<b>bold</b> name<script>x</script>", "command": "<b>kept</b>", "frequency": FAR, "enabled": False})
    create_both("create: only tags in the name", {"name": "<b></b>", "command": "true", "frequency": FAR, "enabled": False})
    create_both("create: name with spaces", {"name": "  padded name  ", "command": "  padded  ", "frequency": " 0 0 1 1 * ", "container": " c ", "enabled": False})
    create_both("create: unicode", {"name": "taak é 日本", "command": "echo é", "frequency": FAR, "enabled": False})
    for frequency in ["every_minute", "hourly", "daily", "weekly", "monthly", "yearly", "@hourly", "@daily", "@weekly", "@monthly", "@yearly", "0 0 * * 7", "0 0 29 2 *",
                      "0-59/2 * * * *", "0 0 * * sun", "0 0 1 dec *"]:
        create_both(f"create: frequency {frequency}", {"name": f"freq {frequency}", "command": "true", "frequency": frequency, "enabled": False})
    create_both("create: enabled as 1", {"name": "enabled-one", "command": "true", "frequency": FAR, "enabled": 1})
    create_both("create: enabled as string", {"name": "enabled-str", "command": "true", "frequency": FAR, "enabled": "1"})
    # A command longer than the column: the query fails.
    create_both("create: command over 255 characters", {"name": "long", "command": "x" * 256, "frequency": FAR, "enabled": False})
    create_both("create: command of 255 characters", {"name": "long-ok", "command": "x" * 255, "frequency": FAR, "enabled": False})
    track_tasks_of_app()
    task_rows("create: rows")

    both("list: by name, root token", "GET", TASKS)
    tiers("list", "GET", TASKS)
    sub.cross_read("list: each side reads the other's rows", TASKS)


def update_cases():
    T1 = TASKS + "/<t1>"
    both_raw("update: no content type", "PATCH", T1, b'{"name":"x"}', None)
    both_raw("update: invalid JSON", "PATCH", T1, b"{nope", "application/json")
    both_raw("update: empty object", "PATCH", T1, b"{}", "application/json")
    both("update: unknown task", "PATCH", TASKS + "/nope", {"name": "x"})
    both("update: unknown task, invalid", "PATCH", TASKS + "/nope", {"name": 5})
    both("update: unknown application", "PATCH", "/applications/nope/scheduled-tasks/<t1>", {"name": "x"})
    both("update: extra field", "PATCH", T1, {"nope": 1})
    both("update: extra and valid", "PATCH", T1, {"nope": 1, "name": "x"})
    both("update: frequency invalid", "PATCH", T1, {"frequency": "garbage"})
    both("update: frequency invalid, unknown task", "PATCH", TASKS + "/nope", {"frequency": "garbage"})
    for key, values in {
        "name": [None, "", 5, "x" * 256],
        "command": [None, "", 5],
        "frequency": [None, "", 5],
        "container": [5, []],
        "timeout": [None, "", 0, 36001, "abc"],
        "enabled": [None, "", "maybe"],
    }.items():
        for value in values:
            both(f"update: {key}={json.dumps(value)[:30]}", "PATCH", T1, {key: value})
    # The disabled task stays writable with a write token; enabling needs deploy.
    T2 = TASKS + "/<t2>"
    tiers("update: an enabled task", "PATCH", T1, {"name": "changed"}, (READ, WRITE, MEMBER_READ, MEMBER_WRITE, OTHER))
    tiers("update: enable", "PATCH", T2, {"enabled": True}, (READ, WRITE, MEMBER_WRITE))
    both("update: write token on a disabled task", "PATCH", T2, {"name": "a-every-write"}, WRITE)
    both("update: write token disables an enabled task", "PATCH", T1, {"enabled": False}, WRITE)
    both("update: enable with root", "PATCH", T1, {"enabled": True})
    both("update: nothing changes", "PATCH", T1, {"name": "z-default"})
    both("update: every field", "PATCH", T1, {"name": "<i>z</i>-renamed", "command": "echo new", "frequency": "@yearly", "container": "c1", "timeout": "60", "enabled": False})
    both("update: container to null", "PATCH", T1, {"container": None})
    both("update: container empty", "PATCH", T1, {"container": ""})
    both("update: enabled string", "PATCH", T1, {"enabled": "1"})
    both("update: enabled off", "PATCH", T1, {"enabled": "false"})
    both("update: enabled zero", "PATCH", T1, {"enabled": 0})
    both("update: command too long", "PATCH", T1, {"command": "y" * 256})
    task_rows("update: rows")
    for side in SIDES:
        status, body = call(side, "GET", fill(side, TASKS).replace("/applications", "/api/v1/applications", 1), None, ROOT_TOK)
        record(f"update: list answers on {side}", status == 200, body[:200])


def delete_and_executions_cases():
    T1 = TASKS + "/<t1>"
    E1 = T1 + "/executions"
    both("executions: none", "GET", E1)
    both("executions: unknown task", "GET", TASKS + "/nope/executions")
    both("executions: unknown application", "GET", "/applications/nope/scheduled-tasks/<t1>/executions")
    tiers("executions", "GET", E1)
    X1 = T1 + "/execute"
    both("execute: unknown task", "POST", TASKS + "/nope/execute")
    both("execute: unknown application", "POST", "/applications/nope/scheduled-tasks/<t1>/execute")
    tiers("execute", "POST", X1)
    both("delete: unknown task", "DELETE", TASKS + "/nope")
    both("delete: unknown application", "DELETE", "/applications/nope/scheduled-tasks/<t1>")
    tiers("delete", "DELETE", TASKS + "/<t2>", None, (READ, MEMBER_READ, MEMBER_WRITE, OTHER))
    both("delete: root", "DELETE", TASKS + "/<t2>")
    both("delete: again", "DELETE", TASKS + "/<t2>")
    both("delete: write token", "DELETE", T1, None, WRITE)
    task_rows("delete: rows")


# ---------------------------------------------------------------------------
# Execution on the VM


def wait_finished(task_id, count=1, seconds=90):
    for _ in range(seconds * 2):
        if sql(f"SELECT count(*) FROM scheduled_task_executions WHERE scheduled_task_id={task_id} AND finished_at IS NOT NULL") == str(count):
            return True
        time.sleep(0.5)
    return False


def wait_running(task_id, seconds=60):
    for _ in range(seconds * 4):
        if sql(f"SELECT count(*) FROM scheduled_task_executions WHERE scheduled_task_id={task_id}") != "0":
            return True
        time.sleep(0.25)
    return False


def run_task(body, wait=True):
    """Makes one disabled task per side through its own API and executes it. Returns {side: task}."""
    tasks = {}
    for side in SIDES:
        status, data = call(side, "POST", fill(side, f"/api/v1/applications/{ctx[side]['run_app']}/scheduled-tasks"), dict(body, enabled=False), ROOT_TOK)
        if status != 201:
            raise SystemExit(f"{side}: could not create the task: {status} {data[:300]}")
        uuid = json.loads(data)["uuid"]
        tasks[side] = {"uuid": uuid, "id": track_task(uuid)}
    for side in SIDES:
        status, data = call(side, "POST", f"/api/v1/applications/{ctx[side]['run_app']}/scheduled-tasks/{tasks[side]['uuid']}/execute", None, ROOT_TOK)
        record(f"run {body['name']}: answer of {side}", status == 200 and json.loads(data) == {"message": "Scheduled task execution queued."}, data[:200])
    if wait:
        for side in SIDES:
            record(f"run {body['name']}: {side} finished", wait_finished(tasks[side]["id"]), sql(f"SELECT status, message FROM scheduled_task_executions WHERE scheduled_task_id={tasks[side]['id']}")[:300])
    return tasks


def execution_rows(task_id):
    text = sql(f"SELECT coalesce(json_agg(row_to_json(e) ORDER BY e.id), '[]')::text FROM scheduled_task_executions e WHERE scheduled_task_id={task_id}")
    return json.loads(text)


def same_execution(label, tasks, normalize=lambda s: s):
    rows = {}
    for side in SIDES:
        items = execution_rows(tasks[side]["id"])
        for row in items:
            if row.get("started_at") is None or row.get("finished_at") is None or row.get("duration") is None or row.get("uuid") is None:
                record(f"{label}: {side} row is complete", False, str(row)[:400])
            if row["created_at"] != row["started_at"]:
                # The clock second can tick between the two on the reference.
                diff = abs(time.mktime(time.strptime(row["created_at"], "%Y-%m-%dT%H:%M:%S")) - time.mktime(time.strptime(row["started_at"], "%Y-%m-%dT%H:%M:%S")))
                if diff > 2:
                    record(f"{label}: {side} created_at", False, str(row)[:400])
            for d in EXEC_DROP:
                row.pop(d, None)
            if isinstance(row.get("message"), str):
                row["message"] = normalize(row["message"])
        rows[side] = items
    record(f"{label}: execution rows", rows["ref"] == rows["rs"], f"ref {str(rows['ref'])[:800]}\n  rs  {str(rows['rs'])[:800]}")
    return rows


def executions_api(label, tasks):
    """The executions of a task as each implementation lists them, the other's rows too."""
    for side in SIDES:
        path = f"/api/v1/applications/{ctx[side]['run_app']}/scheduled-tasks/{tasks[side]['uuid']}/executions"
        for tok in (ROOT_TOK, READ):
            a, b = call("ref", "GET", path, None, tok), call("rs", "GET", path, None, tok)
            record(f"{label}: executions of {side}'s task, {'root' if tok == ROOT_TOK else 'read'} token", a == b, f"ref {a}\n  rs  {b}")


def execution_cases():
    uuid = ctx["ref"]["run_app"]
    name = ctx["ref"]["run_name"]

    # docker exec merges its two streams without a fixed order, in both implementations.
    sort_lines = lambda s: "\n".join(sorted(s.split("\n")))  # noqa: E731
    tasks = run_task({"name": "output", "command": "echo hello; echo to-stderr >&2; cat /etc/hostname", "frequency": FAR})
    rows = same_execution("output", tasks, sort_lines)
    record("output: the command ran in the container", rows["rs"][0]["status"] == "success" and "hello" in rows["rs"][0]["message"], str(rows["rs"])[:300])
    executions_api("output", tasks)

    # What the container sees as its command line.
    same_execution("quoting", run_task({"name": "quoting", "command": "echo 'single' \"double\" $((1+2)) `echo sub`; tr '\\0' ' ' < /proc/$$/cmdline", "frequency": FAR}))
    same_execution("empty output", run_task({"name": "empty", "command": "true", "frequency": FAR}))
    same_execution("null output", run_task({"name": "null", "command": "echo null", "frequency": FAR}))
    same_execution("padded output", run_task({"name": "padded", "command": "printf '  \\n padded \\t\\n\\n'", "frequency": FAR}))
    same_execution("invalid utf-8", run_task({"name": "bytes", "command": "printf 'a\\377b\\n'", "frequency": FAR}))
    same_execution("failing with output", run_task({"name": "failing", "command": "echo some output; echo more >&2; exit 3", "frequency": FAR}), sort_lines)
    same_execution("failing without output", run_task({"name": "silent", "command": "exit 7", "frequency": FAR}))
    same_execution("command not found", run_task({"name": "missing", "command": "definitely-not-a-command", "frequency": FAR}))
    same_execution("secret in the output", run_task({"name": "secret", "command": "echo value-tasksdiffsecretvalue42-end", "frequency": FAR}))
    same_execution("secret in a failure", run_task({"name": "secret2", "command": "echo tasksdiffsecretvalue42; exit 2", "frequency": FAR}))
    same_execution("single container, other name", run_task({"name": "named", "command": "echo named", "frequency": FAR, "container": "something-else"}))
    same_execution("timeout", run_task({"name": "timeout", "command": "sleep 20; echo late", "frequency": FAR, "timeout": 2}))

    # 5MB and the truncation marker: too long to compare in a report line, so by length and digest.
    tasks = run_task({"name": "big", "command": "head -c 6000000 /dev/zero | tr '\\0' a", "frequency": FAR})
    big = {}
    for side in SIDES:
        big[side] = sql(f"SELECT status, length(message), md5(message), right(message, 45) FROM scheduled_task_executions WHERE scheduled_task_id={tasks[side]['id']}")
    record("output over 5MB is truncated the same", big["ref"] == big["rs"] and "truncated at 5MB" in big["rs"], str(big))

    # The command line on the server while it runs.
    seen = {}
    tasks = {}
    for side in SIDES:
        marker = f"tasksdiff-{RUN}-probe"
        one = run_task_one(side, {"name": f"ps {side}", "command": f"sleep 4; echo {marker}", "frequency": FAR})
        tasks[side] = one
        lines = []
        for _ in range(30):
            out = vm(f"ps -eo args | grep -F {marker} | grep -v grep")
            if out:
                lines = sorted(set(out.splitlines()))
                break
            time.sleep(0.2)
        seen[side] = [ln for ln in lines if "docker exec" in ln and not ln.startswith("bash -c") and "ps -eo" not in ln]
        wait_finished(one["id"])
    record("the docker exec line on the server", seen["ref"] == seen["rs"] and bool(seen["rs"]), f"ref {seen['ref']}\n  rs  {seen['rs']}")
    same_execution("probe", tasks)

    # A second running container of the application.
    second = f"{uuid}-second"
    state["containers"].append(second)
    save()
    vm(f"docker run -d --name {second} --label coolify.applicationUuid={uuid} --label coolify.applicationId=1 alpine sleep 600 >/dev/null")
    third = f"{uuid}-preview-3"
    state["containers"].append(third)
    save()
    vm(f"docker run -d --name {third} --label coolify.applicationUuid={uuid} --label coolify.applicationId=1 --label coolify.pullRequestId=3 alpine sleep 600 >/dev/null")
    same_execution("two containers, no name", run_task({"name": "two", "command": "echo two", "frequency": FAR}))
    same_execution("two containers, empty name", run_task({"name": "two-empty", "command": "echo two", "frequency": FAR, "container": ""}))
    same_execution("two containers, exact name", run_task({"name": "two-exact", "command": "cat /etc/hostname | wc -l; echo in-second", "frequency": FAR, "container": second}))
    same_execution("two containers, main name", run_task({"name": "two-main", "command": "echo in-main", "frequency": FAR, "container": name}))
    same_execution("two containers, unknown name", run_task({"name": "two-nope", "command": "echo x", "frequency": FAR, "container": "nope"}))
    same_execution("two containers, the preview's name", run_task({"name": "two-preview", "command": "echo x", "frequency": FAR, "container": third}))
    vm(f"docker rm -f {second} {third} >/dev/null")

    # A stopped second container is not a running one, and a stopped application has none.
    vm(f"docker create --name {second} --label coolify.applicationUuid={uuid} --label coolify.applicationId=1 alpine true >/dev/null")
    same_execution("a stopped second container", run_task({"name": "stopped-second", "command": "echo one", "frequency": FAR}))
    vm(f"docker rm -f {second} >/dev/null")

    for side in SIDES:
        t = {}
        for s2 in SIDES:
            status, data = call(s2, "POST", fill(side, "/api/v1/applications/<app>/scheduled-tasks"), {"name": f"stopped {s2}", "command": "echo x", "frequency": FAR, "enabled": False}, ROOT_TOK)
            u = json.loads(data)["uuid"]
            t[s2] = {"uuid": u, "id": track_task(u)}
            call(s2, "POST", fill(side, f"/api/v1/applications/<app>/scheduled-tasks/{u}/execute"), None, ROOT_TOK)
            record(f"no container: {s2} finished", wait_finished(t[s2]["id"]), "")
        same_execution(f"no container on {side}'s application", t)

    # Execute while the same task runs: a job that finds the overlap lock held does nothing. The
    # reference here has one Horizon worker, so its second job only starts after the first ended and
    # finds the lock free: one or two rows there, depending on the workers. The rs job starts at once.
    counts = {}
    tasks = {}
    for side in SIDES:
        tasks[side] = run_task_one(side, {"name": f"overlap {side}", "command": "sleep 5; echo done", "frequency": FAR})
        wait_running(tasks[side]["id"])
        status, _ = call(side, "POST", f"/api/v1/applications/{ctx[side]['run_app']}/scheduled-tasks/{tasks[side]['uuid']}/execute", None, ROOT_TOK)
        record(f"overlap: second execute on {side} answers", status == 200, status)
        wait_finished(tasks[side]["id"])
        time.sleep(9)
        counts[side] = len(execution_rows(tasks[side]["id"]))
    print(f"  overlap: reference {counts['ref']} executions, rs {counts['rs']}")
    record("overlap: the locked second job does not run on rs", counts["rs"] == 1, counts)
    record("overlap: the reference ran one or two", counts["ref"] in (1, 2), counts)

    # A task deleted during its run: the job still finishes its execution row (no foreign key).
    tasks = {}
    for side in SIDES:
        tasks[side] = run_task_one(side, {"name": f"gone {side}", "command": "sleep 3; echo done", "frequency": FAR})
        wait_running(tasks[side]["id"])
        call(side, "DELETE", f"/api/v1/applications/{ctx[side]['run_app']}/scheduled-tasks/{tasks[side]['uuid']}", None, ROOT_TOK)
        wait_finished(tasks[side]["id"])
    same_execution("a task deleted while running", tasks)


def run_task_one(side, body):
    status, data = call(side, "POST", f"/api/v1/applications/{ctx[side]['run_app']}/scheduled-tasks", dict(body, enabled=False), ROOT_TOK)
    if status != 201:
        raise SystemExit(f"{side}: could not create the task: {status} {data[:300]}")
    uuid = json.loads(data)["uuid"]
    task_id = track_task(uuid)
    status, data = call(side, "POST", f"/api/v1/applications/{ctx[side]['run_app']}/scheduled-tasks/{uuid}/execute", None, ROOT_TOK)
    record(f"run {body['name']}: answer", status == 200, data[:200])
    return {"uuid": uuid, "id": task_id}


def php_command_cases():
    """The script `boundedTaskCommand()` builds, against the literal the Rust unit test expects."""
    code = ("$r = new ReflectionMethod(App\\Jobs\\ScheduledTaskJob::class, 'boundedTaskCommand'); $r->setAccessible(true);"
            "$j = (new ReflectionClass(App\\Jobs\\ScheduledTaskJob::class))->newInstanceWithoutConstructor();"
            "echo md5($r->invoke($j, \"docker exec 'app-1' sh -c 'echo '\\\\''it works'\\\\'''\"));")
    php = tinker(code).splitlines()[-1].strip()
    here = os.path.dirname(os.path.abspath(__file__))
    rust = open(os.path.join(here, "../../crates/coolify-jobs/src/scheduled_task.rs")).read()
    start = rust.index('"output_file=$(mktemp)', rust.index("fn the_command_matches_upstream"))
    end = rust.index('exit $exit_code"', start) + len("exit $exit_code")
    literal = rust[start + 1:end].replace("\\\\", "\0").replace('\\"', '"').replace("\\n", "\n").replace("\0", "\\")
    record("the script of the unit test is boundedTaskCommand()", hashlib.md5(literal.encode()).hexdigest() == php, f"php {php}\n  literal {literal}")


# ---------------------------------------------------------------------------
# Scheduler


def scheduler_cases():
    """Both schedulers over one enabled every-minute task on a running application."""
    enabled = "SELECT count(*) FROM scheduled_tasks WHERE enabled AND frequency = '* * * * *'"
    print("  enabled every-minute tasks before:", sql(enabled))
    uuid = ctx["ref"]["run_app"]
    status, data = call("rs", "POST", f"/api/v1/applications/{uuid}/scheduled-tasks", {"name": "every minute", "command": "date +%s", "frequency": "* * * * *", "timeout": 30}, ROOT_TOK)
    if status != 201:
        raise SystemExit(f"could not create the scheduled task: {status} {data[:300]}")
    task = json.loads(data)["uuid"]
    task_id = track_task(task)
    far = json.loads(call("rs", "POST", f"/api/v1/applications/{uuid}/scheduled-tasks", {"name": "far away", "command": "date", "frequency": "0 0 1 1 *"}, ROOT_TOK)[1])["uuid"]
    far_id = track_task(far)
    named = json.loads(call("rs", "POST", f"/api/v1/applications/{uuid}/scheduled-tasks", {"name": "named frequency", "command": "echo named", "frequency": "every_minute"}, ROOT_TOK)[1])["uuid"]
    named_id = track_task(named)
    start = time.time()
    time.sleep(200)
    for u in (task, named):
        call("rs", "DELETE", f"/api/v1/applications/{uuid}/scheduled-tasks/{u}", None, ROOT_TOK)
    call("rs", "DELETE", f"/api/v1/applications/{uuid}/scheduled-tasks/{far}", None, ROOT_TOK)
    time.sleep(35)
    for label, tid in (("every minute", task_id), ("every_minute name", named_id)):
        rows = execution_rows(tid)
        minutes = [r["created_at"][:16] for r in rows]
        record(f"scheduler: {label} fired", len(rows) >= 2, str(rows)[:400])
        record(f"scheduler: {label} fired once per minute", len(minutes) == len(set(minutes)), str(minutes))
        record(f"scheduler: {label} ran on time and succeeded", all(r["status"] == "success" for r in rows), str(rows)[:600])
        key = f"scheduled-task:{tid}"
        record(f"scheduler: {label} has its state row", sql(f"SELECT count(*) FROM scheduled_job_states WHERE schedule_key='{key}'") == "1", key)
        left = sql(f"SELECT status, count(*) FROM scheduled_job_deliveries WHERE schedule_key='{key}' GROUP BY 1")
        record(f"scheduler: {label} deliveries finished", left in ("",) or "pending" not in left and "claimed" not in left, left)
    record("scheduler: the far away task did not fire", execution_rows(far_id) == [], str(execution_rows(far_id))[:300])
    print(f"  ran for {int(time.time() - start)}s")


# ---------------------------------------------------------------------------


def setup(base):
    image = dict(base, docker_registry_image_name="nginx", docker_registry_image_tag="1.27-alpine", ports_exposes="80", instant_deploy=False)
    for side in SIDES:
        status, raw_body = call(side, "POST", "/api/v1/applications/dockerimage", dict(image, name=f"tasksdiff-{side}"), ROOT_TOK)
        if status != 201:
            raise SystemExit(f"{side}: {status} {raw_body[:300]}")
        uuid = json.loads(raw_body)["uuid"]
        app_id = sql(f"SELECT id FROM applications WHERE uuid='{uuid}'")
        state["apps"].append({"uuid": uuid, "id": app_id})
        save()
        ctx[side].update(app=uuid, app_id=app_id)
    status, raw_body = call("ref", "POST", "/api/v1/applications/dockerimage", dict(image, name="tasksdiff-run"), ROOT_TOK)
    uuid = json.loads(raw_body)["uuid"]
    app_id = sql(f"SELECT id FROM applications WHERE uuid='{uuid}'")
    state["apps"].append({"uuid": uuid, "id": app_id})
    save()
    call("ref", "POST", f"/api/v1/applications/{uuid}/envs", {"key": "TASKSDIFF_SECRET", "value": "tasksdiffsecretvalue42"}, ROOT_TOK)
    return uuid, app_id


def start_running(uuid, app_id):
    status, raw_body = call("ref", "POST", f"/api/v1/applications/{uuid}/start", None, ROOT_TOK)
    dep = json.loads(raw_body).get("deployment_uuid")
    for _ in range(100):
        end = sql(f"SELECT status FROM application_deployment_queues WHERE deployment_uuid='{dep}'")
        if end not in ("queued", "in_progress"):
            break
        time.sleep(3)
    record("the application is deployed by the reference", end == "finished", end)
    for _ in range(40):
        name = vm(f"docker ps --filter label=coolify.applicationUuid={uuid} --format '{{{{.Names}}}}'")
        if name:
            break
        time.sleep(3)
    for side in SIDES:
        ctx[side].update(run_app=uuid, run_id=app_id, run_name=name)
    # The scheduler reads the stored status; the reference's status job sets it on its own schedule.
    for _ in range(60):
        if "running" in sql(f"SELECT status FROM applications WHERE id={app_id}"):
            break
        time.sleep(5)
    return name


def cleanup(st):
    for t in st["tasks"]:
        sql(f"UPDATE scheduled_tasks SET enabled=false WHERE id={t['id']}")
    for app in st["apps"]:
        call("ref", "DELETE", f"/api/v1/applications/{app['uuid']}?delete_volumes=true&delete_connected_networks=true&delete_configurations=true", None, ROOT_TOK)
    uuids = ",".join(f"'{a['uuid']}'" for a in st["apps"]) or "''"
    for _ in range(60):
        if sql(f"SELECT count(*) FROM applications WHERE uuid IN ({uuids})") == "0":
            break
        time.sleep(3)
    left = sql(f"SELECT uuid FROM applications WHERE uuid IN ({uuids})")
    if left:
        print("not deleted through the API:\n" + left)
    ids = ",".join(str(a["id"]) for a in st["apps"]) or "0"
    id_text = ",".join(f"'{a['id']}'" for a in st["apps"]) or "''"
    sql(f"DELETE FROM application_deployment_queues WHERE application_id IN ({id_text})")
    for t in st["tasks"]:
        sql(f"DELETE FROM scheduled_tasks WHERE id={t['id']}")
        sql(f"DELETE FROM scheduled_task_executions WHERE scheduled_task_id={t['id']}")
        sql(f"DELETE FROM scheduled_job_states WHERE schedule_key='scheduled-task:{t['id']}'")
        sql(f"DELETE FROM scheduled_job_deliveries WHERE schedule_key='scheduled-task:{t['id']}'")
    for name in st["containers"]:
        vm(f"docker rm -f {name} >/dev/null 2>&1")
    for app in st["apps"]:
        vm(f"docker ps -a --filter label=coolify.applicationUuid={app['uuid']} -q | xargs -r docker rm -f; rm -rf /data/coolify/applications/{app['uuid']}")
    if st["project"]:
        call("ref", "DELETE", f"/api/v1/projects/{st['project']}", None, ROOT_TOK)
        body = call("ref", "GET", f"/api/v1/projects/{st['project']}", None, ROOT_TOK)[1]
        for env in parsed(body).get("environments", []) if isinstance(parsed(body), dict) else []:
            call("ref", "DELETE", f"/api/v1/projects/{st['project']}/environments/{env['uuid']}", None, ROOT_TOK)
        call("ref", "DELETE", f"/api/v1/projects/{st['project']}", None, ROOT_TOK)


def main():
    global args
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", default="hml20fsj5eles0xfiqidq8j5")
    ap.add_argument("--vm", default="coolify-rs-e2e")
    ap.add_argument("--state", required=True)
    ap.add_argument("--cleanup", action="store_true", help="only delete what the state file lists")
    ap.add_argument("--only", default="", help="comma separated: api,run,scheduler")
    ap.add_argument("--scheduler", action="store_true", help="the rs side runs with COOLIFY_RS_SCHEDULER=on")
    args = ap.parse_args()
    if args.cleanup:
        cleanup(json.load(open(args.state)))
        return
    only = set(filter(None, args.only.split(",")))
    want = lambda k: not only or k in only  # noqa: E731
    project = json.loads(call("ref", "POST", "/api/v1/projects", {"name": f"tasksdiff-{RUN}"}, ROOT_TOK)[1])["uuid"]
    state["project"] = project
    save()
    base = {"server_uuid": args.server, "project_uuid": project, "environment_name": "production"}
    try:
        run_uuid, run_id = setup(base)
        if want("api"):
            php_command_cases()
            print("== api", flush=True)
            list_cases()
            create_validation()
            clear_tasks()
            create_cases()
            update_cases()
            delete_and_executions_cases()
            clear_tasks()
        if want("run") or (args.scheduler and want("scheduler")):
            start_running(run_uuid, run_id)
        if want("run"):
            print("== run", flush=True)
            execution_cases()
        if args.scheduler and want("scheduler"):
            print("== scheduler", flush=True)
            scheduler_cases()
    finally:
        cleanup(state)
    passed = sum(1 for r in results if r)
    print(f"{passed}/{len(results)} checks passed")
    sys.exit(0 if passed == len(results) else 1)


if __name__ == "__main__":
    main()
