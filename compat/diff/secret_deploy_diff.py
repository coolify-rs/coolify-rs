#!/usr/bin/env python3
"""Secret manager values in real deployments, on the test VM, on both sides.

coolify-rs runs with COOLIFY_RS_TEST_ONLY_FAKE_PROVIDERS. The reference runs the same job classes under
tinker with `Http::fake` forwarding to compat/diff/fake_providers.py (see integrations_diff.py), so
nothing leaves this machine. Compares the files on the server and the deployment log lines.

    COOLIFY_RS_BASE=http://127.0.0.1:18091 python3 compat/diff/secret_deploy_diff.py
"""
import json
import subprocess
import sys
import time

from integrations_diff import ENVIRONMENT, PROJECT, RS, SERVER, FAKE_PHP, api, compare, created, cleanup, login, ref_create_token, results, record, sql, tinker, token_id, fake

VM = "coolify-rs-e2e"


def vm(cmd):
    return subprocess.run(["orb", "-m", VM, "-u", "root", "bash", "-c", cmd], capture_output=True, text=True, timeout=120).stdout


def wait_rs(app_id, dep_uuid):
    for _ in range(90):
        status = sql(f"SELECT status FROM application_deployment_queues WHERE deployment_uuid = '{dep_uuid}'")
        if status in ("finished", "failed", "cancelled-by-user"):
            return status
        time.sleep(2)
    return "timeout"


def log_lines(dep_uuid):
    raw = sql(f"SELECT logs FROM application_deployment_queues WHERE deployment_uuid = '{dep_uuid}'")
    return [e["output"] for e in json.loads(raw or "[]") if not e.get("hidden")]


def deploy_rs(app_uuid, app_id):
    code, body = api("POST", f"/applications/{app_uuid}/start", None)
    if code != 200:
        raise SystemExit(f"start: {code} {body}")
    dep = body["deployment_uuid"]
    return dep, wait_rs(app_id, dep)


def deploy_ref(app_id):
    php = f"""
\\Illuminate\\Support\\Facades\\Queue::fake();
$a = \\App\\Models\\Application::find({app_id});
$uuid = (string) new \\Visus\\Cuid2\\Cuid2;
queue_application_deployment(application: $a, deployment_uuid: $uuid, force_rebuild: false);
$q = \\App\\Models\\ApplicationDeploymentQueue::where('deployment_uuid', $uuid)->first();
$q->update(['status' => 'in_progress']);
try {{ (new \\App\\Jobs\\ApplicationDeploymentJob($q->id))->handle(); }} catch (\\Throwable $e) {{ $x = new \\App\\Jobs\\ApplicationDeploymentJob($q->id); $x->failed($e); }}
echo '<<<'.json_encode(['uuid' => $uuid, 'status' => $q->fresh()->status]).'>>>';"""
    r = tinker(php)
    return r["uuid"], r["status"]


def env_file(app_uuid):
    return vm(f"cat /data/coolify/applications/{app_uuid}/.env 2>/dev/null")


def only_secret_lines(text):
    return sorted(line for line in text.split("\n") if line.split("=")[0] in ("API_KEY", "DB_PASSWORD", "MIXED", "PLAIN", "LITERAL_NOTE"))


def secret_log(lines):
    wanted = ("Fetched ", "Missing secret keys", "Failed to fetch secrets", "Could not fetch secrets", "no secret manager source")
    return [line for line in lines if line.startswith(wanted) or "Could not fetch" in line]


def main():
    ui = login()
    team = int(sql(f"SELECT team_id FROM projects WHERE uuid = '{PROJECT}'"))
    assert team
    ok_token = ref_create_token("doppler", "diff-deploy", "dp.st.good", ["secrets"], {})
    bad_token = ref_create_token("doppler", "diff-deploy-bad", "dp.st.bad", ["secrets"], {})

    def app(name, envs, token_uuid):
        code, body = api("POST", "/applications/dockerimage", {
            "project_uuid": PROJECT, "environment_uuid": ENVIRONMENT, "server_uuid": SERVER,
            "docker_registry_image_name": "nginx", "docker_registry_image_tag": "alpine", "ports_exposes": "80", "name": name,
            "instant_deploy": False,
        })
        if code != 201:
            raise SystemExit(f"create: {code} {body}")
        uuid = body["uuid"]
        app_id = int(sql(f"SELECT id FROM applications WHERE uuid = '{uuid}'"))
        created["applications"].append(app_id)
        for key, value, literal in envs:
            c, b = api("POST", f"/applications/{uuid}/envs", {"key": key, "value": value, "is_literal": literal})
            if c != 201:
                raise SystemExit(f"env: {c} {b}")
        if token_uuid:
            c, b = ui("PUT", f"/ui/resources/application/{uuid}/secret-manager", {"integration_token_uuid": token_uuid})
            assert c == 200, b
        return uuid, app_id

    env_ok = [("API_KEY", "{{vault.API_KEY}}", False), ("DB_PASSWORD", "{{ vault.DB_PASSWORD }}", False),
              ("MIXED", "pre-{{vault.API_KEY}}-post", False), ("PLAIN", "x$y", False)]
    cases = [
        ("values", env_ok, ok_token["uuid"]),
        ("missing key", [("API_KEY", "{{vault.NOPE}}", False)], ok_token["uuid"]),
        ("provider refuses", [("API_KEY", "{{vault.API_KEY}}", False)], bad_token["uuid"]),
        ("no source", [("API_KEY", "{{vault.API_KEY}}", False)], None),
        ("literal without source", [("LITERAL_NOTE", "{{vault.API_KEY}}", True)], None),
    ]
    for name, envs, tok in cases:
        uuid, app_id = app(f"diff-secret-{name.replace(' ', '-')}", envs, tok)
        fake("/__reset", "POST")
        dep_rs, status_rs = deploy_rs(uuid, app_id)
        rs_requests = [(e["method"], e["path"], e["query"]) for e in fake("/__log")]
        rs_env, rs_log = only_secret_lines(env_file(uuid)), secret_log(log_lines(dep_rs))
        rs_leak = '{"leak": false}'
        all_rs = "\n".join(log_lines(dep_rs))
        vm(f"cd /data/coolify/applications/{uuid} 2>/dev/null && docker compose down --remove-orphans >/dev/null 2>&1; rm -f /data/coolify/applications/{uuid}/.env")
        fake("/__reset", "POST")
        dep_ref, status_ref = deploy_ref(app_id)
        ref_requests = [(e["method"], e["path"], e["query"]) for e in fake("/__log")]
        ref_env, ref_log = only_secret_lines(env_file(uuid)), secret_log(log_lines(dep_ref))
        vm(f"cd /data/coolify/applications/{uuid} 2>/dev/null && docker compose down --remove-orphans >/dev/null 2>&1; rm -rf /data/coolify/applications/{uuid}")
        compare(f"{name}: status", status_ref, status_rs)
        compare(f"{name}: .env lines", ref_env, rs_env)
        compare(f"{name}: log lines", ref_log, rs_log)
        compare(f"{name}: provider requests", ref_requests, rs_requests)
        record(f"{name}: secret values hidden in the log", "abc#123" not in all_rs, rs_leak)
        sql(f"DELETE FROM application_deployment_queues WHERE deployment_uuid IN ('{dep_rs}', '{dep_ref}')")

    same = sum(1 for _, s in results if s)
    print(f"\n{same}/{len(results)} same")
    return 0 if same == len(results) else 1


if __name__ == "__main__":
    try:
        code = main()
    finally:
        cleanup()
    sys.exit(code)
