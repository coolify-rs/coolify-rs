#!/usr/bin/env python3
"""Deployments from private git sources: upstream vs coolify-rs, against
compat/diff/fake_git_provider.py (FAKE_BASE, reachable from the reference and the test VM).
Upstream's GitHub App clone URL drops the port and path of html_url, so that case starts a small
socat container on the `coolify` network of the VM, which forwards port 80 to the fake.

Each case creates two identical applications through upstream's models, deploys one with each
implementation on the test VM and compares rows, visible log lines, commands (tokens included:
the fake's tokens are fixed), the Compose file and the answer through the proxy. It proves the
clone commands and token handling against the fake only, not against a real provider.

    FAKE_BASE=http://<address>:18990 COOLIFY_RS_BASE=http://127.0.0.1:18085 git_source_deploy_diff.py --case github,gitlab,gitea,dockerfile
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

FAKE = os.environ["FAKE_BASE"]
SERVER = "hml20fsj5eles0xfiqidq8j5"
ENV = "uj3st9yvtvzxh7rul3c3opcz"
VM = "coolify-rs-e2e"
PREFIX = "srcdeploy"
PROXY = "srcdeploy-fake"


def php(code):
    r = subprocess.run(["docker", "exec", "coolify-rs-reference-coolify-1", "php", "/var/www/html/artisan", "tinker", "--execute", code],
                       capture_output=True, text=True, check=True)
    return r.stdout.strip()


def vm(cmd):
    return subprocess.run(["orb", "-m", VM, "-u", "root", "bash", "-c", cmd], capture_output=True, text=True).stdout


def setup(case):
    extra = {
        "dockerfile": "'dockerfile' => \"FROM nginx:alpine\\nEXPOSE 80\\n\", 'git_repository' => 'coollabsio/coolify', 'repository_project_id' => 0, 'source_id' => 0, 'source_type' => App\\Models\\GithubApp::class,",
        "gitlab-compose": "'git_repository' => 'o/r', 'source_id' => $src->id, 'source_type' => $src->getMorphClass(), 'docker_compose_location' => '/docker-compose.yaml', 'base_directory' => '/',",
        "gitea-compose": "'git_repository' => 'o/r', 'source_id' => $src->id, 'source_type' => $src->getMorphClass(), 'docker_compose_location' => '/docker-compose.yaml', 'base_directory' => '/',",
    }.get(case, "'git_repository' => 'o/r', 'source_id' => $src->id, 'source_type' => $src->getMorphClass(),")
    pack = "dockercompose" if case.endswith("-compose") else "dockerfile"
    out = php(f"""
$server = App\\Models\\Server::where('uuid', '{SERVER}')->first();
$team = $server->team_id;
$key = App\\Models\\PrivateKey::where('name', '{PREFIX}-key')->first() ?? App\\Models\\PrivateKey::create(['name' => '{PREFIX}-key', 'team_id' => $team, 'private_key' => App\\Models\\PrivateKey::generateNewKeyPair('rsa')['private_key']]);
$src = match ('{case.split("-")[0]}') {{
    'github' => App\\Models\\GithubApp::create(['name' => '{PREFIX}-gh', 'team_id' => $team, 'api_url' => '{FAKE}/github', 'html_url' => 'http://{PROXY}', 'app_id' => 4242, 'installation_id' => 99, 'client_id' => 'c', 'client_secret' => 's', 'webhook_secret' => 'w', 'private_key_id' => $key->id]),
    'gitlab' => App\\Models\\GitlabApp::create(['name' => '{PREFIX}-gl', 'team_id' => $team, 'api_url' => '{FAKE}/gitlab/api/v4', 'html_url' => '{FAKE}/gl', 'client_id' => 'c', 'client_secret' => 's', 'access_token' => 'fixed+token', 'refresh_token' => 'r', 'expires_at' => time() + 90000, 'webhook_token' => 'w']),
    'gitea' => App\\Models\\GiteaApp::create(['name' => '{PREFIX}-gt', 'team_id' => $team, 'api_url' => '{FAKE}/gitea/api/v1', 'html_url' => '{FAKE}/gt', 'client_id' => 'c', 'client_secret' => 's', 'access_token' => 'fixed+token', 'refresh_token' => 'r', 'expires_at' => time() + 90000, 'webhook_secret' => 'w']),
    default => null,
}};
$env = App\\Models\\Environment::where('uuid', '{ENV}')->first();
$dest = $server->standaloneDockers()->first();
foreach (['ref', 'rs'] as $side) {{
    $a = new App\\Models\\Application;
    $a->forceFill([{extra} 'name' => '{PREFIX}-'.$side, 'git_branch' => 'main', 'build_pack' => '{pack}', 'ports_exposes' => '80', 'environment_id' => $env->id, 'destination_id' => $dest->id, 'destination_type' => $dest->getMorphClass(), 'health_check_enabled' => false]);
    $a->save();
    $a->fqdn = 'http://'.$a->uuid.'.'.$server->ip.'.sslip.io';
    $a->save();
    echo $side.'='.$a->uuid.PHP_EOL;
}}
""")
    return dict(line.split("=") for line in out.splitlines() if "=" in line)


def teardown(uuids):
    for u in uuids:
        call("ref", "DELETE", f"/api/v1/applications/{u}?delete_volumes=true", tok="acme_owner_root")
    time.sleep(8)
    php(f"""
foreach (['github_apps', 'gitlab_apps', 'gitea_apps'] as $t) {{ DB::table($t)->where('name', 'like', '{PREFIX}-%')->delete(); }}
App\\Models\\PrivateKey::where('name', '{PREFIX}-key')->get()->each(fn ($k) => $k->delete());
""")


def deployments(u):
    rows = sql(f"SELECT row_to_json(q)::text FROM application_deployment_queues q WHERE application_id=(SELECT id::text FROM applications WHERE uuid='{u}') ORDER BY id")
    return [json.loads(r) for r in rows.splitlines() if r.strip()]


def wait(u):
    for _ in range(300):
        d = deployments(u)
        if d and d[0]["status"] in ("finished", "failed", "cancelled-by-user") and d[0]["finished_at"]:
            time.sleep(4)
            return deployments(u)[0]
        time.sleep(2)
    return (deployments(u) or [None])[0]


def norm(text, pairs):
    for old, new in pairs:
        if old:
            text = text.replace(old, new)
    text = re.sub(r"-\d{8}[Tt]\d{6}", "-TS", text)
    text = re.sub(r'time="[^"]*"', 'time="T"', text)
    text = re.sub(r"coolify\.build-fingerprint=[0-9a-f]{64}", "coolify.build-fingerprint=FP", text)
    return re.sub(r"COOLIFY_BUILD_SECRETS_HASH=[0-9a-f]{64}", "COOLIFY_BUILD_SECRETS_HASH=H", text)


def entries(q, u, side):
    pairs = [(u, "U"), (q["deployment_uuid"], "DEP"), (q.get("commit"), "SHA"), (f"{PREFIX}-{side}", "NAME")]
    out = json.loads(q["logs"] or "[]")
    visible = [(e["type"], norm(e["output"], pairs)) for e in out if not e["hidden"] and not (e.get("command") is None and (e["output"].startswith("Container ") or e["output"].startswith("time=")))]
    commands, seen = [], set()
    for e in out:
        c = e.get("command")
        if c and (e["batch"], c) not in seen:
            seen.add((e["batch"], c))
            commands.append(norm(c, pairs))
    return visible, commands


def run(case):
    if case == "github":
        target = FAKE.split("://", 1)[1]
        vm(f"docker rm -f {PROXY} >/dev/null 2>&1; docker run -d --name {PROXY} --network coolify alpine/socat tcp-listen:80,fork,reuseaddr tcp:{target}")
    apps = setup(case)
    ur, us = apps["ref"], apps["rs"]
    try:
        for side, u in (("ref", ur), ("rs", us)):
            c, r = call(side, "POST", f"/api/v1/deploy?uuid={u}", {}, tok="acme_owner_root")
            record(f"[{case}] deploy accepted by {side}", c == 200, f"{c} {r}")
        qr, qs = wait(ur), wait(us)
        record(f"[{case}] both finished", qr and qs and qr["status"] == qs["status"] == "finished", f"{qr and qr['status']} / {qs and qs['status']}")
        if not (qr and qs):
            return
        skip = {"id", "application_id", "deployment_uuid", "created_at", "updated_at", "finished_at", "deployment_url", "application_name",
                "horizon_job_id", "horizon_job_worker", "current_process_id", "logs", "configuration_snapshot", "configuration_hash", "configuration_diff", "is_api"}
        diff = {k: (qr[k], qs.get(k)) for k in qr if k not in skip and qr[k] != qs.get(k)}
        record(f"[{case}] rows alike", not diff, diff)
        (vr, cr), (vs, cs) = entries(qr, ur, "ref"), entries(qs, us, "rs")
        record(f"[{case}] visible log entries identical", vr == vs, "\n".join(difflib.unified_diff([f"{t} {o}" for t, o in vr], [f"{t} {o}" for t, o in vs], lineterm="")))
        record(f"[{case}] logged commands identical", cr == cs, "\n".join(difflib.unified_diff(cr, cs, lineterm="")))
        fr = norm(vm(f"cat /data/coolify/applications/{ur}/docker-compose.yaml"), [(ur, "U"), (f"{PREFIX}-ref", "NAME")])
        fs = norm(vm(f"cat /data/coolify/applications/{us}/docker-compose.yaml"), [(us, "U"), (f"{PREFIX}-rs", "NAME")])
        record(f"[{case}] docker-compose.yaml identical", fr == fs and fr != "", "\n".join(difflib.unified_diff(fr.splitlines(), fs.splitlines(), lineterm="")))
        if case.endswith("-compose"):
            return
        codes = []
        for u in (ur, us):
            host = f"{u}.{sql(f'SELECT ip FROM servers WHERE uuid = {chr(39)}{SERVER}{chr(39)}')}.sslip.io"
            codes.append(vm(f"curl -s -o /dev/null -w '%{{http_code}}' -H 'Host: {host}' http://127.0.0.1/"))
        record(f"[{case}] both answer through the proxy", codes[0] == codes[1] == "200", codes)
    finally:
        teardown([ur, us])
        if case == "github":
            vm(f"docker rm -f {PROXY}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--case", default="github,gitlab,gitea,dockerfile")
    for case in ap.parse_args().case.split(","):
        run(case)
    failed = results.count(False)
    print(f"\n{len(results) - failed}/{len(results)} checks identical")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
