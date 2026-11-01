#!/usr/bin/env python3
"""GitHub runner provisioning: a signed `workflow_job` webhook on each side, against
compat/diff/fake_git_provider.py and the test VM, with a fake runner image on the VM (the JIT config
is never used, so nothing reaches GitHub). Compares the provider requests, the responses, the rows and
what `docker inspect` shows of the runner. Everything is tracked by id and deleted at the end.

    python3 compat/diff/fake_git_provider.py --port 18990 &
    FAKE_BASE=http://<lan address>:18990 COOLIFY_RS_BASE=http://127.0.0.1:18091 python3 compat/diff/github_runners_diff.py
"""
import hashlib
import hmac
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
RS = os.environ.get("COOLIFY_RS_BASE", "http://127.0.0.1:18091")
REF = "http://127.0.0.1:18000"
FAKE = os.environ["FAKE_BASE"]
SERVER = "hml20fsj5eles0xfiqidq8j5"
VM = "coolify-rs-e2e"
IMAGE = "coolify-rs-fake-runner:local"
SECRET = "runner-webhook-secret"
results = []
state = {"app": None, "key": None, "config": None, "executions": []}


def compare(name, ref, rs):
    same = ref == rs
    results.append(same)
    print(("same  " if same else "DIFF  ") + name)
    if not same:
        print(f"      ref={json.dumps(ref, sort_keys=True)[:1400]}\n      rs ={json.dumps(rs, sort_keys=True)[:1400]}")


def sql(q):
    r = subprocess.run(["docker", "exec", "coolify-rs-reference-postgres-1", "psql", "-U", "coolify", "-d", "coolify", "-At", "-c", q], capture_output=True, text=True)
    if r.returncode:
        raise SystemExit(r.stderr)
    return r.stdout.strip()


def php(code):
    r = subprocess.run(["docker", "exec", "coolify-rs-reference-coolify-1", "php", "/var/www/html/artisan", "tinker", "--execute", code], capture_output=True, text=True, check=True)
    return r.stdout.strip()


def vm(cmd):
    return subprocess.run(["orb", "-m", VM, "-u", "root", "bash", "-c", cmd], capture_output=True, text=True, timeout=180).stdout


def fake(path, method="GET"):
    req = urllib.request.Request(FAKE + path, method=method, data=b"" if method == "POST" else None)
    with urllib.request.urlopen(req) as r:
        return json.loads(r.read() or b"null")


def hook(base, payload):
    raw = json.dumps(payload).encode()
    sig = hmac.new(SECRET.encode(), raw, hashlib.sha256).hexdigest()
    req = urllib.request.Request(base + "/webhooks/source/github/events", data=raw, method="POST")
    req.add_header("Content-Type", "application/json")
    req.add_header("X-GitHub-Event", "workflow_job")
    req.add_header("X-GitHub-Delivery", "d-" + str(time.time()))
    req.add_header("X-GitHub-Hook-Installation-Target-Id", "4242")
    req.add_header("X-Hub-Signature-256", "sha256=" + sig)
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return r.status, r.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()[:300]


def payload(action, job, runner="", run_id=5, labels=("self-hosted", "gpu")):
    return {
        "action": action, "installation": {"id": 99}, "repository": {"full_name": "o/r"},
        "workflow_job": {"id": job, "run_id": run_id, "labels": list(labels), "runner_name": runner, "name": "build", "workflow_name": "CI",
                         "html_url": "http://fake/job", "conclusion": "success" if action == "completed" else None},
    }


def execution(job):
    raw = sql(f"SELECT row_to_json(e)::text FROM github_runner_executions e WHERE github_app_id = {state['app']} AND trigger_workflow_job_id = {job}")
    return json.loads(raw) if raw else None


def wait_status(job, statuses, seconds=120):
    for _ in range(seconds):
        e = execution(job)
        if e and e["status"] in statuses:
            return e
        time.sleep(1)
    return execution(job)


def inspect(uuid):
    name = f"coolify-runner-{uuid}"
    raw = vm(f"docker inspect {name} 2>/dev/null")
    data = json.loads(raw or "[]")
    if not data:
        return None
    c = data[0]
    env = [x.split("=")[0] for x in c["Config"]["Env"] if x.split("=")[0].startswith(("ACTIONS_", "DOCKER_HOST", "RUNNER_WAIT"))]
    return {
        "image": c["Config"]["Image"], "cmd": c["Config"]["Cmd"],
        "labels": sorted(k + "=" + v.replace(uuid, "U") for k, v in c["Config"]["Labels"].items() if k.startswith("coolify.")),
        "networks": [n.replace(uuid, "U") for n in c["NetworkSettings"]["Networks"]],
        "pids": c["HostConfig"]["PidsLimit"], "memory": c["HostConfig"]["Memory"], "cpus": c["HostConfig"]["NanoCpus"],
        "binds": [b.replace(uuid, "U") for b in (c["HostConfig"]["Binds"] or [])],
        "env": sorted(env),
        "jit": any(x == "ACTIONS_RUNNER_INPUT_JITCONFIG=bm90LWEtcmVhbC1qaXQtY29uZmln" for x in c["Config"]["Env"]),
        "secret_file_gone": vm(f"ls /data/coolify/github-runners/{uuid}.env* 2>/dev/null").strip() == "",
    }


def requests():
    """The provider requests after the token and clock calls, which both sides make before them."""
    return [(e["method"], e["path"], e.get("json")) for e in fake("/__log") if "/access_tokens" not in e["path"] and e["path"] != "/github/zen"]


def row_view(e):
    keep = ("status", "workflow_job_id", "workflow_job_html_url", "workflow_name", "job_name", "repository_full_name", "labels", "conclusion", "is_pull_request", "provision_attempts", "error_message", "runner_id")
    out = {k: e.get(k) for k in keep}
    out["workflow_job_id"] = e.get("workflow_job_id") == e.get("trigger_workflow_job_id")
    out["runner_name_is_container"] = e.get("runner_name") == f"coolify-runner-{e['uuid']}"
    for t in ("ready_at", "started_at", "completed_at", "queued_at"):
        out[t] = e.get(t) is not None
    return out


def flow(side, base, job, allow_pr, pull_request):
    out = {}
    fake("/__reset", "POST")
    run_id = 77 if pull_request else 5
    out["queued"] = hook(base, payload("queued", job, run_id=run_id))
    e = wait_status(job, ("idle", "failed", "cancelled"), 150)
    if pull_request and not allow_pr:
        out["execution"] = e
        out["requests"] = requests()
        return out
    state["executions"].append(e["id"])
    out["idle_row"] = row_view(e)
    out["inspect"] = inspect(e["uuid"])
    out["requests_provision"] = requests()
    name = e["runner_name"]
    out["in_progress"] = hook(base, payload("in_progress", job, runner=name))
    out["running_row"] = row_view(wait_status(job, ("running",), 10))
    fake("/__reset", "POST")
    out["completed"] = hook(base, payload("completed", job, runner=name))
    final = wait_status(job, ("completed",), 20)
    out["final_row"] = row_view(final)
    for _ in range(30):
        if not vm(f"docker ps -a -q --filter name=coolify-runner-{e['uuid']}").strip() and not vm(f"docker network ls -q --filter name=coolify-runner-{e['uuid']}").strip():
            break
        time.sleep(1)
    out["gone"] = vm(f"docker ps -a -q --filter name=coolify-runner-{e['uuid']}").strip() == ""
    time.sleep(2)
    out["requests_cleanup"] = requests()
    return out


def main():
    vm(f"""mkdir -p /tmp/fake-runner && cd /tmp/fake-runner && printf 'FROM alpine\\nRUN mkdir -p /home/runner/_work && printf "#!/bin/sh\\\\nsleep 600\\\\n" > /home/runner/run.sh && chmod +x /home/runner/run.sh\\n' > Dockerfile && docker build -q -t {IMAGE} . >/dev/null; rm -rf /tmp/fake-runner""")
    out = php(f"""
$server = App\\Models\\Server::where('uuid', '{SERVER}')->first();
$key = App\\Models\\PrivateKey::create(['name' => 'rundiff-key', 'team_id' => $server->team_id, 'private_key' => App\\Models\\PrivateKey::generateNewKeyPair('rsa')['private_key']]);
$app = App\\Models\\GithubApp::create(['name' => 'rundiff-gh', 'team_id' => $server->team_id, 'api_url' => '{FAKE}/github', 'html_url' => '{FAKE}/gh', 'app_id' => 4242, 'installation_id' => 99, 'client_id' => 'c', 'client_secret' => 's', 'webhook_secret' => '{SECRET}', 'private_key_id' => $key->id, 'organization' => 'acme']);
$c = App\\Models\\GithubRunnerConfig::create(['server_id' => $server->id, 'github_app_id' => $app->id, 'labels' => ['gpu'], 'is_enabled' => true, 'max_runners' => 3, 'runner_image' => '{IMAGE}', 'cpu_limit' => '1', 'memory_limit' => '256m', 'allow_pull_requests' => false]);
echo 'ids='.$app->id.','.$key->id.','.$c->id.PHP_EOL;""")
    ids = [line for line in out.splitlines() if line.startswith("ids=")][0][4:].split(",")
    state.update(app=int(ids[0]), key=int(ids[1]), config=int(ids[2]))

    for name, ref_job, rs_job in [("push job", 7001, 7002)]:
        ref = flow("ref", REF, ref_job, False, False)
        # The group created on the first run stays on the App, like upstream: start the second side alike.
        sql(f"UPDATE github_apps SET runner_group_id = NULL WHERE id = {state['app']}")
        rs = flow("rs", RS, rs_job, False, False)
        sql(f"UPDATE github_apps SET runner_group_id = NULL WHERE id = {state['app']}")
        for key in ref:
            if key == "requests_provision":
                strip = lambda r: [(m, p, ({**b, "name": "N"} if b and p.endswith(("generate-jitconfig", "runner-groups")) else b)) for m, p, b in r]
                compare(f"{name}: {key}", strip(ref[key]), strip(rs[key]))
            else:
                compare(f"{name}: {key}", ref[key], rs[key])
    # A pull request job with a config that refuses them: no execution.
    for label, base, job in (("ref", REF, 7003), ("rs", RS, 7004)):
        pass
    ref_r = hook(REF, payload("queued", 7003, run_id=77))
    rs_r = hook(RS, payload("queued", 7004, run_id=77))
    compare("pull request job refused", ref_r, rs_r)
    compare("no execution for it", execution(7003), execution(7004))
    # Labels that match nothing, another installation, a completed job without a runner.
    wrong = payload("queued", 7005, labels=("self-hosted",))
    compare("no matching labels", hook(REF, wrong), hook(RS, wrong))
    other = {**payload("queued", 7006), "installation": {"id": 99}}
    other["workflow_job"]["id"] = 0
    compare("no job id", hook(REF, other), hook(RS, other))
    done = payload("completed", 7007)
    compare("completed without a runner", hook(REF, done), hook(RS, done))

    same = results.count(True)
    print(f"\n{same}/{len(results)} same")
    return 0 if same == len(results) else 1


def cleanup():
    if state["app"]:
        for e in sql(f"SELECT uuid FROM github_runner_executions WHERE github_app_id = {state['app']}").split():
            vm(f"docker rm -f coolify-runner-{e} >/dev/null 2>&1; docker network rm coolify-runner-{e} >/dev/null 2>&1; rm -f /data/coolify/github-runners/{e}*")
        sql(f"DELETE FROM github_runner_executions WHERE github_app_id = {state['app']}")
    if state["config"]:
        sql(f"DELETE FROM github_runner_configs WHERE id = {state['config']}")
    if state["app"]:
        sql(f"DELETE FROM github_apps WHERE id = {state['app']}")
    if state["key"]:
        sql(f"DELETE FROM private_keys WHERE id = {state['key']}")
    vm(f"docker image rm {IMAGE} >/dev/null 2>&1")


if __name__ == "__main__":
    import signal
    for sig in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, lambda *_: sys.exit(1))
    try:
        code = main()
    finally:
        cleanup()
    sys.exit(code)
