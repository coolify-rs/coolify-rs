#!/usr/bin/env python3
"""`ReconcileGithubRunnersJob`, run directly on each side, on the test VM and against
compat/diff/fake_git_provider.py. Seeds executions in every state and one stray runner container, then
compares what each row and container turned into.

    python3 compat/diff/fake_git_provider.py --port 18990 &
    FAKE_BASE=http://<lan address>:18990 python3 compat/diff/github_runners_reconcile_diff.py

coolify-rs runs through `coolify-rs server-job reconcile-github-runners`, with the instance binary of
port 18087 and the same ssh wrapper as scripts/dev-server.sh. Rows and containers are tracked and removed.
"""
import json
import os
import subprocess
import sys

from dns_flows_diff import rs_env
from github_runners_diff import FAKE, SECRET, SERVER, compare, php, results, sql, vm

BIN = os.path.expanduser("~/.cache/coolify-rs-dev/18087/coolify-rs")
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
state = {"app": None, "key": None, "config": None, "executions": []}


def seed_rows(side):
    """One execution for each branch of failureFor and reconcileQueued."""
    sid = sql(f"SELECT id FROM servers WHERE uuid = '{SERVER}'")
    a, c = state["app"], state["config"]
    cases = {
        "provisioning-old": ("provisioning", "now() - interval '1 day'", None, None),
        "provisioning-new": ("provisioning", "now()", None, None),
        "running-gone": ("running", "now()", None, "now()"),
        "running-old": ("running", "now()", None, "now() - interval '2 days'"),
        "running-ok": ("running", "now()", None, "now()"),
        "idle-old": ("idle", "now()", "now() - interval '2 days'", None),
        "idle-ok": ("idle", "now()", "now()", None),
    }
    out = {}
    for i, (name, (status, updated, ready, started)) in enumerate(cases.items()):
        uuid = f"rcl{side}{i}"
        row = sql(f"INSERT INTO github_runner_executions (uuid, github_app_id, github_runner_config_id, server_id, status, trigger_workflow_job_id, runner_name, ready_at, started_at, provision_attempts, queued_at, created_at, updated_at, is_pull_request) "
                  f"VALUES ('{uuid}', {a}, {c}, {sid}, '{status}', {9000 + i}, 'r{i}', {ready or 'NULL'}, {started or 'NULL'}, 0, now(), now(), {updated}, false) RETURNING id").split()[0]
        state["executions"].append(int(row))
        out[name] = uuid
        if name not in ("running-gone", "provisioning-old", "provisioning-new"):
            vm(f"docker run -d --name coolify-runner-{uuid} --label coolify.githubRunner=true --label coolify.githubRunnerExecution={uuid} alpine sleep 600 >/dev/null")
    q = f"rcl{side}q"
    row = sql(f"INSERT INTO github_runner_executions (uuid, github_app_id, github_runner_config_id, server_id, status, trigger_workflow_job_id, provision_attempts, queued_at, created_at, updated_at, is_pull_request) "
              f"VALUES ('{q}', {a}, {c}, NULL, 'queued', 9100, 0, now() - interval '2 days', now(), now(), false) RETURNING id").split()[0]
    state["executions"].append(int(row))
    out["queued-old"] = q
    stray = f"rcl{side}stray"
    vm(f"docker run -d --name coolify-runner-{stray} --label coolify.githubRunner=true --label coolify.githubRunnerExecution={stray} alpine sleep 600 >/dev/null")
    out["stray"] = stray
    return out


def view(uuids):
    rows = {}
    for name, uuid in uuids.items():
        r = sql(f"SELECT status, error_message FROM github_runner_executions WHERE uuid = '{uuid}'")
        c = vm(f"docker ps -a --filter name=^/coolify-runner-{uuid}$ --format '{{{{.State}}}}'").strip()
        rows[name] = {"row": r, "container": c}
    return rows


def clear_side(uuids):
    for uuid in uuids.values():
        vm(f"docker rm -f coolify-runner-{uuid} >/dev/null 2>&1; docker network rm coolify-runner-{uuid} >/dev/null 2>&1; rm -f /data/coolify/github-runners/{uuid}*")
        sql(f"DELETE FROM github_runner_executions WHERE uuid = '{uuid}'")


def main():
    out = php(f"""
$server = App\\Models\\Server::where('uuid', '{SERVER}')->first();
$key = App\\Models\\PrivateKey::create(['name' => 'rcldiff-key', 'team_id' => $server->team_id, 'private_key' => App\\Models\\PrivateKey::generateNewKeyPair('rsa')['private_key']]);
$app = App\\Models\\GithubApp::create(['name' => 'rcldiff-gh', 'team_id' => $server->team_id, 'api_url' => '{FAKE}/github', 'html_url' => '{FAKE}/gh', 'app_id' => 4243, 'installation_id' => 98, 'client_id' => 'c', 'client_secret' => 's', 'webhook_secret' => '{SECRET}', 'private_key_id' => $key->id, 'organization' => 'acme']);
$c = App\\Models\\GithubRunnerConfig::create(['server_id' => $server->id, 'github_app_id' => $app->id, 'labels' => ['gpu'], 'is_enabled' => true, 'max_runners' => 9, 'runner_image' => 'alpine', 'cpu_limit' => '1', 'memory_limit' => '256m', 'allow_pull_requests' => false, 'idle_timeout' => 60, 'job_timeout' => 60, 'capacity_wait_timeout' => 60]);
echo 'ids='.$app->id.','.$key->id.','.$c->id.PHP_EOL;""")
    ids = [line for line in out.splitlines() if line.startswith("ids=")][0][4:].split(",")
    state.update(app=int(ids[0]), key=int(ids[1]), config=int(ids[2]))

    uuids = seed_rows("r")
    out = php("""
\\Illuminate\\Support\\Facades\\Bus::fake([\\App\\Jobs\\CleanupGithubRunnerJob::class, \\App\\Jobs\\ProvisionGithubRunnerJob::class]);
(new \\App\\Jobs\\ReconcileGithubRunnersJob)->handle();
echo 'cleanup='.\\Illuminate\\Support\\Facades\\Bus::dispatched(\\App\\Jobs\\CleanupGithubRunnerJob::class)->count();""")
    ref = view(uuids)
    print("reference:", out[-80:])
    clear_side(uuids)

    uuids = seed_rows("s")
    env = rs_env()
    if subprocess.run(["nc", "-G", "2", "-z", "coolify-rs-e2e.orb.local", "22"], capture_output=True).returncode:
        env["COOLIFY_RS_SSH_BINARY"] = os.path.join(ROOT, "scripts/ssh-via-orb.sh")
    run = subprocess.run([BIN, "server-job", "reconcile-github-runners", SERVER], capture_output=True, text=True, timeout=120, env=env)
    print("coolify-rs:", run.stdout[-100:], run.stderr[-200:])
    rs = view(uuids)
    clear_side(uuids)
    # CleanupGithubRunnerJob is held back upstream, and runs after the command here: a finished runner's
    # container is not compared.
    for name in ref:
        compare(f"reconcile: {name} row", ref[name]["row"], rs[name]["row"])
    for name in ("running-ok", "idle-ok", "stray"):
        compare(f"reconcile: {name} container", ref[name]["container"], rs[name]["container"])
    same = results.count(True)
    print(f"\n{same}/{len(results)} same")
    return 0 if same == len(results) else 1


def cleanup():
    if state["app"]:
        for e in sql(f"SELECT uuid FROM github_runner_executions WHERE github_app_id = {state['app']}").split():
            vm(f"docker rm -f coolify-runner-{e} >/dev/null 2>&1; rm -f /data/coolify/github-runners/{e}*")
        sql(f"DELETE FROM github_runner_executions WHERE github_app_id = {state['app']}")
    if state["config"]:
        sql(f"DELETE FROM github_runner_configs WHERE id = {state['config']}")
    if state["app"]:
        sql(f"DELETE FROM github_apps WHERE id = {state['app']}")
    if state["key"]:
        sql(f"DELETE FROM private_keys WHERE id = {state['key']}")


if __name__ == "__main__":
    try:
        code = main()
    finally:
        cleanup()
    sys.exit(code)
