#!/usr/bin/env python3
"""Differential check of git deployments from a public repository: upstream vs coolify-rs.

Same setup as applications_diff.py. Usage:

    git_deploy_diff.py --server <server uuid> --vm <orbstack machine name> [--case dockerfile,static,nixpacks,railpack]

Per case it creates two identical applications from coollabsio/coolify-examples through
the reference API, deploys one with the reference and one with coolify-rs, and compares
the deployment rows, the visible log entries, the commands of the hidden entries, the
Docker Compose file, the container environment and the answer through the proxy. Then it
deploys both again (the build is skipped) and compares those logs too. Everything it
creates is deleted at the end. Exit status 1 when anything differs.
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
from databases_diff import call, record, results, sql  # noqa: E402

REPO = "https://github.com/coollabsio/coolify-examples"
INLINE_DOCKERFILE = base64.b64encode(b"FROM nginx:alpine\nARG DIFF_ARG=x\nRUN echo inline > /usr/share/nginx/html/index.html\nHEALTHCHECK --interval=5s CMD wget -q -O- http://127.0.0.1/ || exit 1\n").decode()
INLINE_SECRET_DOCKERFILE = base64.b64encode(b"FROM nginx:alpine AS base\nRUN echo \"$DIFF_SECRET\" > /usr/share/nginx/html/index.html\nFROM base\nRUN env > /usr/share/nginx/html/env.html\nHEALTHCHECK --interval=5s CMD wget -q -O- http://127.0.0.1/ || exit 1\n").decode()
# An older commit of the repository that still has dockerfile/single-stage.
ROLLBACK_COMMIT = "ff425d7999a5bb92bc0e977e89567d68c2a6b45b"
CASES = {
    "dockerfile": dict(build_pack="dockerfile", base_directory="/dockerfile/single-stage", ports_exposes="80"),
    "static": dict(build_pack="static", base_directory="/static", ports_exposes="80"),
    "nixpacks": dict(build_pack="nixpacks", base_directory="/javascript/runtimes/nodejs", ports_exposes="3000"),
    "railpack": dict(build_pack="railpack", base_directory="/javascript/runtimes/nodejs", ports_exposes="3000"),
    "compose": dict(build_pack="dockercompose", base_directory="/internal/compose", docker_compose_location="/docker-compose.yaml"),
    "compose-simple": dict(build_pack="dockercompose", base_directory="/internal/compose-parser-tests",
                           docker_compose_location="/docker-compose-simple.yaml"),
    # Custom Docker options in the Compose file and the helper's seccomp mount; {seccomp} is a profile on the VM.
    "custom-options": dict(build_pack="dockerfile", base_directory="/dockerfile/single-stage", ports_exposes="80",
                           custom_docker_run_options="--cap-add=NET_ADMIN --cap-drop SYS_TIME --init --hostname diff-host --ulimit nofile=1024:2048 "
                           "--shm-size 64m --dns 1.1.1.1 --add-host a.test:1.2.3.4 --tmpfs /run/x --pids-limit 200 "
                           "--sysctl net.core.somaxconn=1024 -it --network-alias zzz -e FOO=bar --security-opt seccomp={seccomp}"),
    # --ip stops the rolling update; each side needs its own address ({ip}).
    "custom-ip": dict(build_pack="dockerfile", base_directory="/dockerfile/single-stage", ports_exposes="80",
                      custom_docker_run_options="--ip {ip}"),
    # Pre- and post-deployment commands: deploy 1 has no old container, deploy 2 does.
    "commands": dict(build_pack="dockerfile", base_directory="/dockerfile/single-stage", ports_exposes="80",
                     pre_deployment_command="echo 'pre output' && hostname",
                     post_deployment_command="echo post output\nid -u"),
    "post-fail": dict(build_pack="dockerfile", base_directory="/dockerfile/single-stage", ports_exposes="80", post_deployment_command="echo oops; exit 3"),
    "post-fail-silent": dict(build_pack="dockerfile", base_directory="/dockerfile/single-stage", ports_exposes="80", post_deployment_command="exit 4"),
    "pre-fail": dict(build_pack="dockerfile", base_directory="/dockerfile/single-stage", ports_exposes="80", pre_deployment_command="echo bad; exit 5"),
    "pre-named": dict(build_pack="dockercompose", base_directory="/internal/compose-parser-tests", docker_compose_location="/docker-compose-simple.yaml", pre_deployment_command="hostname", pre_deployment_command_container="api",
                      post_deployment_command="hostname", post_deployment_command_container="analytics"),
    "pre-missing": dict(build_pack="dockercompose", base_directory="/internal/compose-parser-tests", docker_compose_location="/docker-compose-simple.yaml", pre_deployment_command="hostname", pre_deployment_command_container="nope"),
    "pre-unnamed": dict(build_pack="dockercompose", base_directory="/internal/compose-parser-tests", docker_compose_location="/docker-compose-simple.yaml", pre_deployment_command="hostname"),
    "post-missing": dict(build_pack="dockercompose", base_directory="/internal/compose-parser-tests", docker_compose_location="/docker-compose-simple.yaml", post_deployment_command="hostname", post_deployment_command_container="nope"),
    "post-unnamed": dict(build_pack="dockercompose", base_directory="/internal/compose-parser-tests", docker_compose_location="/docker-compose-simple.yaml", post_deployment_command="hostname"),
    # HTTP basic auth: labels saved at create, and generated by the job when none are stored.
    "basic-auth": dict(build_pack="dockerfile", base_directory="/dockerfile/single-stage", ports_exposes="80", is_http_basic_auth_enabled=True, http_basic_auth_username="diffuser", http_basic_auth_password="diff-pass-1"),
    "basic-auth-generated": dict(build_pack="dockerfile", base_directory="/dockerfile/single-stage", ports_exposes="80", is_http_basic_auth_enabled=True, http_basic_auth_username="diffuser", http_basic_auth_password="diff-pass-1"),
    # File storages (STORAGES below): the file is missing, or something else is at its path.
    "file-storage": dict(build_pack="dockerfile", base_directory="/dockerfile/single-stage", ports_exposes="80"),
    "file-storage-blocked": dict(build_pack="dockerfile", base_directory="/dockerfile/single-stage", ports_exposes="80"),
    "file-storage-empty-dir": dict(build_pack="dockerfile", base_directory="/dockerfile/single-stage", ports_exposes="80"),
    # Each side creates the row, so the parser of coolify-rs inserts the file mounts and saves them.
    "compose-files": dict(build_pack="dockercompose", base_directory="/internal/compose-parser-tests",
                          docker_compose_location="/docker-compose-local-volumes.yaml"),
    # An inline Dockerfile from POST /applications/dockerfile on each side's own API.
    "inline": dict(dockerfile=INLINE_DOCKERFILE, ports_exposes="80"),
    "inline-instant": dict(dockerfile=INLINE_DOCKERFILE, ports_exposes="80", instant_deploy=True),
    # Deploy 2 rolls back to the commit of deploy 1 (the image exists), deploy 3 to an older commit (ROLLBACK_COMMIT).
    "rollback": dict(build_pack="dockerfile", base_directory="/dockerfile/single-stage", ports_exposes="80"),
    # A deploy key against a git-over-SSH mirror of the repository (--deploy-key, --ssh-repo).
    "deploykey": dict(build_pack="dockerfile", base_directory="/dockerfile/single-stage", ports_exposes="80"),
    # Build secrets (PREPARE_SQL, ENVS): the helper holds the values, the Dockerfile mounts them.
    "secrets": dict(build_pack="dockerfile", base_directory="/dockerfile/single-stage", ports_exposes="80"),
    "secrets-inline": dict(dockerfile=INLINE_SECRET_DOCKERFILE, ports_exposes="80"),
    "secrets-nixpacks": dict(build_pack="nixpacks", base_directory="/javascript/runtimes/nodejs", ports_exposes="3000"),
    "secrets-compose": dict(build_pack="dockercompose", base_directory="/internal/compose", docker_compose_location="/docker-compose.yaml"),
    # Preserved repository: `docker cp` of the clone into the configuration directory.
    "preserve": dict(build_pack="dockerfile", base_directory="/dockerfile/single-stage", ports_exposes="80"),
    "preserve-compose": dict(build_pack="dockercompose", base_directory="/internal/compose-parser-tests",
                             docker_compose_location="/docker-compose-local-volumes.yaml"),
    # Custom Compose commands, with and without the flags upstream injects.
    "custom-commands": dict(build_pack="dockercompose", base_directory="/internal/compose", docker_compose_location="/docker-compose.yaml",
                            docker_compose_custom_build_command="docker compose --project-name {name} build --pull",
                            docker_compose_custom_start_command="docker compose --project-name {name} up -d"),
    "custom-flags": dict(build_pack="dockercompose", base_directory="/internal/compose", docker_compose_location="/docker-compose.yaml",
                         docker_compose_custom_build_command="docker compose --project-name {name} -f internal/compose/docker-compose.yaml --env-file=/artifacts/build-time.env --project-directory internal/compose build",
                         docker_compose_custom_start_command="docker compose --project-name {name} --project-directory internal/compose up -d --remove-orphans"),
    "custom-preserve": dict(build_pack="dockercompose", base_directory="/internal/compose-parser-tests", docker_compose_location="/docker-compose-simple.yaml",
                            docker_compose_custom_start_command="docker compose --project-name {name} up -d"),
    "custom-raw": dict(build_pack="dockercompose", base_directory="/internal/compose-parser-tests", docker_compose_location="/docker-compose-simple.yaml",
                       docker_compose_custom_start_command="docker compose --project-name {name} up -d"),
    "raw": dict(build_pack="dockercompose", base_directory="/internal/compose-parser-tests", docker_compose_location="/docker-compose-simple.yaml"),
    "custom-secrets": dict(build_pack="dockercompose", base_directory="/internal/compose", docker_compose_location="/docker-compose.yaml",
                           docker_compose_custom_build_command="docker compose --project-name {name} build"),
    "custom-build-fail": dict(build_pack="dockercompose", base_directory="/internal/compose", docker_compose_location="/docker-compose.yaml",
                              docker_compose_custom_build_command="docker compose nosuchcommand"),
    "custom-start-fail": dict(build_pack="dockercompose", base_directory="/internal/compose-parser-tests", docker_compose_location="/docker-compose-simple.yaml",
                              docker_compose_custom_start_command="docker compose nosuchcommand"),
    # Previews through POST /deploy?pr=N (PREVIEWS: the pull request of the repository), after a production deployment.
    "preview-dockerfile": dict(build_pack="dockerfile", base_directory="/dockerfile/single-stage", ports_exposes="80"),
    "preview-static": dict(build_pack="static", base_directory="/static", ports_exposes="80"),
    "preview-secrets": dict(build_pack="dockerfile", base_directory="/dockerfile/single-stage", ports_exposes="80"),
    "preview-nixpacks": dict(build_pack="nixpacks", base_directory="/javascript/runtimes/nodejs", ports_exposes="3000"),
    # A Docker Image preview: deploy 2 is POST /deploy?pr=N&docker_tag=..., which creates the row.
    "preview-image": dict(docker_registry_image_name="nginx", docker_registry_image_tag="alpine", ports_exposes="80"),
    # Stored past the API's validation: the job refuses it.
    "custom-unsafe": dict(build_pack="dockercompose", base_directory="/internal/compose-parser-tests", docker_compose_location="/docker-compose-simple.yaml"),
}


def norm(text, pairs):
    for old, new in pairs:
        if old:
            text = text.replace(old, new)
    text = re.sub(r"-\d{8}[Tt]\d{6}", "-TS", text)
    # Basic auth password hashes are salted.
    text = re.sub(r"\${1,2}2y\${1,2}\d+\${1,2}[./A-Za-z0-9]{53}", "HASH", text)
    text = re.sub(r'time="[^"]*"', 'time="T"', text)
    # Railpack writes the build time into the image.
    text = re.sub(r"RAILPACK_BUILT_AT=\d+", "RAILPACK_BUILT_AT=T", text)
    # The Compose build fingerprint hashes per-application inputs.
    text = re.sub(r"coolify\.build-fingerprint=[0-9a-f]{64}", "coolify.build-fingerprint=FP", text)
    # A preview image before the commit is known carries the deployment uuid.
    text = re.sub(r"pr-(\d+)-[a-z0-9]{24}", r"pr-\1-DEP", text)
    text = re.sub(r"COOLIFY_BUILD_SECRETS_HASH(=|: )[0-9a-f]{64}", r"COOLIFY_BUILD_SECRETS_HASH\1H", text)
    return text


def sorted_mounts(text):
    """The Compose file with each run of bind mount lines sorted."""
    out, run = [], []
    for line in text.splitlines():
        if line.lstrip().startswith("- '/data/coolify/applications/"):
            run.append(line)
            continue
        out.extend(sorted(run))
        run = []
        out.append(line)
    return "\n".join(out + sorted(run))


# POST /applications/{uuid}/storages on each side's own API, then a shell command on the VM ({file}: the path of the first).
STORAGE_BODIES = [{"type": "file", "mount_path": "/etc/diff/app.conf", "content": "hello storage"},
                  {"type": "file", "mount_path": "/etc/diff/data", "is_directory": True, "fs_path": "etc/diff/data"},
                  {"type": "file", "mount_path": "/etc/diff/empty.conf"}]
STORAGES = {"file-storage": (STORAGE_BODIES, "rm -f {file}"),
            "preserve": (STORAGE_BODIES, "true"),
            "file-storage-blocked": (STORAGE_BODIES, "rm -f {file} && mkdir -p {file} && touch {file}/x"),
            "file-storage-empty-dir": (STORAGE_BODIES, "rm -f {file} && mkdir -p {file}")}
# Each side creates the application through its own API.
OWN_CREATE = ("compose-files", "inline", "inline-instant", "preserve-compose")
# Queued by the create request, so the first deployment needs no call.
INSTANT = ("inline-instant",)
# How many deployments a case runs, when it is not two.
DEPLOYS = {"rollback": 3, "preview-dockerfile": 3}
# Each side deletes the application through its own API, and the result is compared.
OWN_DELETE = ("file-storage", "compose-files", "preserve", "preserve-compose")
# SQL on the created row before the first deployment; {u} is its uuid.
SETTING = "UPDATE application_settings SET {col} = true WHERE application_id = (SELECT id FROM applications WHERE uuid = '{u}')"
BUILD_SECRETS = SETTING.replace("{col}", "use_build_secrets")
PRESERVE = SETTING.replace("{col}", "is_preserve_repository_enabled")
PREPARE_SQL = {"basic-auth-generated": "UPDATE applications SET custom_labels = NULL WHERE uuid = '{u}'",
               "secrets": BUILD_SECRETS, "secrets-inline": BUILD_SECRETS, "secrets-nixpacks": BUILD_SECRETS, "secrets-compose": BUILD_SECRETS,
               "custom-secrets": BUILD_SECRETS, "preserve": PRESERVE, "preserve-compose": PRESERVE, "custom-preserve": PRESERVE,
               "custom-raw": SETTING.replace("{col}", "is_raw_compose_deployment_enabled"),
               "raw": SETTING.replace("{col}", "is_raw_compose_deployment_enabled"),
               "preview-secrets": BUILD_SECRETS,
               "custom-unsafe": "UPDATE applications SET docker_compose_custom_start_command = 'echo $(id)' WHERE uuid = '{u}'"}
# POST /applications/{uuid}/envs through the reference, for both rows.
SECRET_ENV = [{"key": "DIFF_SECRET", "value": "hunter2-secret", "is_buildtime": True, "is_runtime": True},
              {"key": "DIFF_MULTI", "value": "line one\nline two", "is_multiline": True, "is_buildtime": True, "is_runtime": False}]
ENVS = {"preview-dockerfile": SECRET_ENV, "preview-secrets": SECRET_ENV, "secrets": SECRET_ENV, "secrets-inline": SECRET_ENV, "secrets-nixpacks": SECRET_ENV[:1], "secrets-compose": SECRET_ENV[:1]}
# Pull request deployed by deploy 2 of these cases, with the commit it deploys at the head of the repository.
PREVIEW_PR = 74
# The tree of that pull request is older: the application's base directory for the preview.
PREVIEW_BASE = {"preview-dockerfile": "/dockerfile", "preview-secrets": "/dockerfile", "preview-static": "/static", "preview-nixpacks": "/nodejs"}
PREVIEW_CASES = ("preview-dockerfile", "preview-static", "preview-secrets", "preview-nixpacks", "preview-image")
# Cases that also compare the hidden log: build.sh and the steps BuildKit prints, which show the rewritten Dockerfile.
HIDDEN_CASES = {"secrets", "secrets-inline", "secrets-nixpacks", "secrets-compose", "custom-commands", "custom-flags", "custom-secrets"}
# A file in the running image: what the build saw.
IMAGE_FILES = {"secrets": "/usr/share/nginx/html/env.html", "secrets-inline": "/usr/share/nginx/html/env.html"}
# The status each deployment of a case ends with, when it is not `finished`.
EXPECTED_STATUS = {("pre-fail", 2): "failed", ("pre-missing", 2): "failed", ("pre-unnamed", 2): "failed",
                   ("custom-build-fail", 1): "failed", ("custom-build-fail", 2): "failed", ("custom-start-fail", 1): "failed",
                   ("custom-start-fail", 2): "failed", ("custom-unsafe", 1): "failed", ("custom-unsafe", 2): "failed",
                   # `cd` in the helper, then a Compose file path that only exists on the server.
                   ("custom-raw", 1): "failed", ("custom-raw", 2): "failed"}
# Every deployment fails, so nothing runs and no file is written.
NOTHING_RUNS = ("custom-build-fail", "custom-start-fail", "custom-unsafe", "custom-raw")
# Cases that wait for the status pollers: the columns they compare do not depend on the other cases.
STATUS_CASES = ("dockerfile", "static", "nixpacks", "railpack", "deploykey")
SECCOMP_PROFILE = '{"defaultAction":"SCMP_ACT_ALLOW"}'
# What the Compose file and the Docker options change on the container.
HOST_CONFIG = ("{{json .HostConfig.CapAdd}} {{json .HostConfig.CapDrop}} {{json .HostConfig.Init}} {{json .HostConfig.Ulimits}} "
               "{{json .HostConfig.ShmSize}} {{json .HostConfig.Dns}} {{json .HostConfig.ExtraHosts}} {{json .HostConfig.Tmpfs}} "
               "{{json .HostConfig.PidsLimit}} {{json .HostConfig.Sysctls}} {{json .HostConfig.SecurityOpt}} {{json .Config.Hostname}} "
               "{{json .Config.Tty}} {{json .Config.OpenStdin}} {{json .Config.Entrypoint}} {{json .HostConfig.Binds}}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", required=True)
    ap.add_argument("--vm", required=True)
    ap.add_argument("--case", default=",".join(c for c in CASES if c != "deploykey"))
    ap.add_argument("--deploy-key", help="private key file the --ssh-repo accepts")
    ap.add_argument("--ssh-repo", help="e.g. git@192.168.139.220:examples.git")
    ap.add_argument("--track", help="file that receives each created uuid as it is created")
    a = ap.parse_args()

    def vm(cmd):
        return subprocess.run(["orb", "-m", a.vm, "-u", "root", "bash", "-c", cmd], capture_output=True, text=True).stdout

    def deployments(u):
        rows = sql(f"SELECT row_to_json(q)::text FROM application_deployment_queues q WHERE application_id=(SELECT id::text FROM applications WHERE uuid='{u}') ORDER BY id")
        return [json.loads(r) for r in rows.splitlines() if r.strip()]

    def wait(u, count):
        for _ in range(300):
            d = deployments(u)
            if len(d) >= count and d[count - 1]["status"] in ("finished", "failed", "cancelled-by-user") and d[count - 1]["finished_at"]:
                # The finally block logs and stops the helper after finished_at.
                dep = d[count - 1]["deployment_uuid"]
                for _ in range(60):
                    q = deployments(u)[count - 1]
                    if "Gracefully shutting down build container" in (q["logs"] or "") and not vm(f"docker ps -aq --filter name=^{dep}$").strip():
                        break
                    time.sleep(2)
                return deployments(u)[count - 1]
            time.sleep(2)
        return (deployments(u) or [None])[-1]

    def deploy(side, u):
        c, r = call(side, "POST", f"/api/v1/deploy?uuid={u}", {}, tok="acme_owner_root")
        return c, r

    project = json.loads(call("ref", "POST", "/api/v1/projects", {"name": "git-deploy-diff"})[1])["uuid"]
    base = {"server_uuid": a.server, "project_uuid": project, "environment_name": "production",
            "git_repository": REPO, "git_branch": "main", "instant_deploy": False}
    created = []
    key_uuid = None
    run_id = f"{os.getpid()}-{int(time.time())}"
    seccomp = f"/tmp/git-deploy-diff-{run_id}.json"
    try:
        if "deploykey" in a.case.split(","):
            c, r = call("ref", "POST", "/api/v1/security/keys",
                        {"name": "git-deploy-diff", "private_key": open(a.deploy_key).read()}, tok="acme_owner_root")
            key_uuid = json.loads(r)["uuid"]
        for case in a.case.split(","):
            template = CASES[case]
            apps = {}
            ips = {}
            if "{seccomp}" in json.dumps(template):
                vm(f"echo '{SECCOMP_PROFILE}' > {seccomp}")
            if "{ip}" in json.dumps(template):
                subnet = vm("docker network inspect coolify --format '{{(index .IPAM.Config 0).Subnet}}'").strip()
                prefix = subnet.rsplit(".", 1)[0]
                ips = {"ref": f"{prefix}.201", "rs": f"{prefix}.202"}
            for side in ("ref", "rs"):
                spec = {k: v.replace("{seccomp}", seccomp).replace("{ip}", ips.get(side, "")).replace("{name}", f"{case}-{side}") if isinstance(v, str) else v for k, v in template.items()}
                if "docker_registry_image_name" in spec:
                    body = dict(server_uuid=a.server, project_uuid=project, environment_name="production", name=f"{case}-{side}",
                                **{"instant_deploy": False, **spec})
                    c, r = call(side, "POST", "/api/v1/applications/dockerimage", body, tok="acme_owner_root")
                elif "dockerfile" in spec:
                    body = dict(server_uuid=a.server, project_uuid=project, environment_name="production", name=f"{case}-{side}",
                                **{"instant_deploy": False, **spec})
                    c, r = call(side, "POST", "/api/v1/applications/dockerfile", body, tok="acme_owner_root")
                elif case == "deploykey":
                    body = dict(base, name=f"{case}-{side}", git_repository=a.ssh_repo, private_key_uuid=key_uuid, **spec)
                    c, r = call("ref", "POST", "/api/v1/applications/private-deploy-key", body, tok="acme_owner_root")
                else:
                    creator = side if case in OWN_CREATE else "ref"
                    c, r = call(creator, "POST", "/api/v1/applications/public", dict(base, name=f"{case}-{side}", **spec), tok="acme_owner_root")
                body = json.loads(r)
                if c != 201:
                    record(f"[{case}] create", False, r)
                    break
                created.append(body["uuid"])
                if a.track:
                    with open(a.track, "a") as f:
                        f.write(body["uuid"] + "\n")
                apps[side] = body
            if len(apps) < 2:
                continue
            ur, us = apps["ref"]["uuid"], apps["rs"]["uuid"]
            if spec.get("build_pack") == "dockercompose":
                # Creating queues upstream's LoadComposeFile job; deploy after it stored the file.
                for _ in range(60):
                    if all(sql(f"SELECT docker_compose_raw IS NOT NULL FROM applications WHERE uuid='{u}'") == "t" for u in (ur, us)):
                        break
                    time.sleep(2)
            if case in PREPARE_SQL:
                for u in (ur, us):
                    sql(PREPARE_SQL[case].replace("{u}", u))
            if (case in ENVS or case in PREPARE_SQL) and spec.get("build_pack") == "dockercompose":
                # The job that stored the file still creates the SERVICE_* and storage rows; their order is compared.
                time.sleep(15)
            for body in ENVS.get(case, []):
                for u in (ur, us):
                    c, r = call("ref", "POST", f"/api/v1/applications/{u}/envs", body, tok="acme_owner_root")
                    if c != 201:
                        record(f"[{case}] env accepted", False, f"{c} {r}")
            if case in STORAGES:
                bodies, shell = STORAGES[case]
                for side, u in (("ref", ur), ("rs", us)):
                    for body in bodies:
                        c, r = call(side, "POST", f"/api/v1/applications/{u}/storages", body, tok="acme_owner_root")
                        if c != 201:
                            record(f"[{case}] storage accepted by {side}", False, f"{c} {r}")
                    first = f"/data/coolify/applications/{u}{bodies[0]['mount_path']}"
                    # ServerStorageSaveJob runs after the response.
                    for _ in range(30):
                        if vm(f"test -f {first} && echo y").strip() == "y":
                            break
                        time.sleep(2)
                    record(f"[{case}] {side} saved the file on the server", vm(f"cat {first}") == bodies[0]["content"])
                    vm(shell.replace("{file}", first))
            first_commit = None
            for n in range(1, DEPLOYS.get(case, 2) + 1):
                for side, u in (("ref", ur), ("rs", us)):
                    if n == 1 and case in INSTANT:
                        continue
                    if n == 2 and case in PREVIEW_BASE:
                        sql(f"UPDATE applications SET base_directory = '{PREVIEW_BASE[case]}' WHERE uuid = '{u}'")
                    if case == "rollback" and n > 1:
                        target = first_commit if n == 2 else ROLLBACK_COMMIT
                        c, r = call(side, "POST", f"/api/v1/applications/{u}/rollback", {"commit": target}, tok="acme_owner_root")
                    elif case == "preview-image" and n == 2:
                        c, r = call(side, "POST", f"/api/v1/deploy?uuid={u}&pr={PREVIEW_PR}&docker_tag=1.27-alpine", {}, tok="acme_owner_root")
                    elif case in PREVIEW_CASES and n == 2:
                        c, r = call(side, "POST", f"/api/v1/applications/{u}/previews", {"pull_request_id": PREVIEW_PR, "pull_request_html_url": f"{REPO}/pull/{PREVIEW_PR}", "git_type": "github", "instant_deploy": True}, tok="acme_owner_root")
                        c = 200 if c == 201 else c
                    elif case in PREVIEW_CASES and n == 3:
                        c, r = call(side, "POST", f"/api/v1/deploy?uuid={u}&pr={PREVIEW_PR}", {}, tok="acme_owner_root")
                    else:
                        c, r = deploy(side, u)
                    if c != 200:
                        record(f"[{case}] deploy {n} accepted by {side}", False, f"{c} {r}")
                qr, qs = wait(ur, n), wait(us, n)
                label = f"[{case}] deploy {n}"
                expected = EXPECTED_STATUS.get((case, n), "finished")
                record(f"{label}: both {expected}", qr and qs and qr["status"] == qs["status"] == expected,
                       f"{qr and qr['status']} / {qs and qs['status']}")
                if not (qr and qs):
                    continue
                skip = {"id", "application_id", "deployment_uuid", "created_at", "updated_at", "finished_at", "deployment_url",
                        "application_name", "horizon_job_id", "horizon_job_worker", "current_process_id", "logs",
                        "configuration_snapshot", "configuration_hash", "configuration_diff", "is_api"}
                record(f"{label}: rows alike", {k: v for k, v in qr.items() if k not in skip} == {k: v for k, v in qs.items() if k not in skip},
                       {k: (qr[k], qs.get(k)) for k in qr if k not in skip and qr[k] != qs.get(k)})
                commit = qr.get("commit")
                first_commit = first_commit or commit
                if case == "rollback" and n > 1:
                    target = first_commit if n == 2 else ROLLBACK_COMMIT
                    record(f"{label}: rollback rows", all(q["rollback"] and q["commit"] == target and not q["force_rebuild"] for q in (qr, qs)),
                           [(q["rollback"], q["commit"], q["force_rebuild"]) for q in (qr, qs)])

                def entries(q, u):
                    pairs = [(u, "U"), (q["deployment_uuid"], "DEP"), (commit, "SHA"), (f"{case}-ref", "NAME"), (f"{case}-rs", "NAME")]
                    pairs += [(ip, "IP") for ip in ips.values()]
                    out = json.loads(q["logs"] or "[]")
                    visible, compose_lines = [], []
                    starting = False
                    for e in out:
                        if e["hidden"]:
                            continue
                        text = norm(e["output"], pairs)
                        # `docker ps` order of the containers of an application.
                        text = re.sub(r"Available: (.*)$", lambda m: "Available: " + ", ".join(sorted(m[1].split(", "))), text)
                        # How many attempts see "starting" depends on timing.
                        if text.startswith("Attempt "):
                            starting = '"starting"' in text
                            text = re.sub(r"^Attempt \d+", "Attempt N", text)
                        if starting:
                            continue
                        # `docker compose up` output: the line order varies between runs.
                        if e.get("command") is None and (text.startswith("Container ") or text.startswith("time=")) :
                            compose_lines.extend(line.strip() for line in text.splitlines())
                        else:
                            visible.append((e["type"], text))
                    visible.append(("compose-up", "\n".join(sorted(compose_lines))))
                    commands, seen = [], set()
                    for e in out:
                        cmd = e.get("command")
                        # One pair of these per healthcheck attempt.
                        if cmd and ".State.Health." in cmd:
                            if cmd in seen:
                                continue
                            seen.add(cmd)
                        if cmd and (e["batch"], cmd) not in seen:
                            seen.add((e["batch"], cmd))
                            # The helper container id and hashed build args differ by run.
                            commands.append(re.sub(r"COOLIFY_BUILD_SECRETS_HASH=[0-9a-f]{64}", "COOLIFY_BUILD_SECRETS_HASH=H", norm(cmd, pairs)))
                    # Old containers stop in `docker ps` order, which is arbitrary for containers started together.
                    ordered, run = [], []
                    for c in commands:
                        if c.startswith("docker stop --timeout") and "'DEP'" not in c or c.startswith("bash -c 'if command -v timeout"):
                            run.append(c)
                            continue
                        ordered.extend(sorted(run))
                        run = []
                        ordered.append(c)
                    ordered.extend(sorted(run))
                    return visible, ordered
                def hidden(q, u):
                    pairs = [(u, "U"), (q["deployment_uuid"], "DEP"), (commit, "SHA"), (f"{case}-ref", "NAME"), (f"{case}-rs", "NAME")]
                    text = "\n".join(e["output"] for e in json.loads(q["logs"] or "[]") if e["hidden"] and not e.get("command"))
                    lines = []
                    for line in norm(text, pairs).splitlines():
                        step = re.match(r"#\d+ (\[.*)", line)
                        if step:
                            lines.append(step[1])
                        elif "docker build" in line or line.startswith("cd /artifacts/"):
                            lines.append(line)
                    return sorted(set(lines))
                if case in HIDDEN_CASES:
                    hr, hs = hidden(qr, ur), hidden(qs, us)
                    record(f"{label}: build steps and build.sh identical", hr == hs, "\n".join(difflib.unified_diff(hr, hs, lineterm="")))
                (vr, cr), (vs, cs) = entries(qr, ur), entries(qs, us)
                record(f"{label}: visible log entries identical", vr == vs,
                       "\n".join(difflib.unified_diff([f"{t} {o}" for t, o in vr], [f"{t} {o}" for t, o in vs], lineterm="")))
                record(f"{label}: logged commands identical", cr == cs,
                       "\n".join(difflib.unified_diff(cr, cs, lineterm="")))
            if case in IMAGE_FILES:
                seen = []
                for u in (ur, us):
                    name = vm(f"docker ps --filter label=coolify.applicationUuid={u} --format '{{{{.Names}}}}'").split()
                    text = vm(f"docker exec {name[0]} cat {IMAGE_FILES[case]}") if name else ""
                    # The build container's hostname and working directory.
                    seen.append(sorted(norm(l, [(u, "U"), (f"{case}-ref", "NAME"), (f"{case}-rs", "NAME")]) for l in text.splitlines() if not l.startswith(("HOSTNAME=", "SOURCE_COMMIT="))))
                record(f"[{case}] the build saw the same variables", seen[0] == seen[1] and seen[0] != [], "\n".join(difflib.unified_diff(seen[0], seen[1], lineterm="")))
                record(f"[{case}] the build saw the secret", any("hunter2-secret" in l for l in seen[0]), seen[0])
            if case in PREVIEW_CASES:
                seen = []
                time.sleep(10)  # GetContainersStatus, dispatched by the end of the deployment
                for u in (ur, us):
                    row = sql(f"SELECT row_to_json(p)::text FROM application_previews p WHERE application_id = (SELECT id FROM applications WHERE uuid = '{u}') AND pull_request_id = {PREVIEW_PR}")
                    row = json.loads(row or "{}")
                    prod = vm(f"docker ps -a --filter label=coolify.applicationUuid={u} --filter label=coolify.pullRequestId=0 --format '{{{{.State}}}} {{{{.Image}}}}'").strip()
                    pr = vm(f"docker ps -a --filter label=coolify.applicationUuid={u} --filter label=coolify.pullRequestId={PREVIEW_PR} --format '{{{{.Names}}}} {{{{.State}}}} {{{{.Image}}}}'").strip()
                    labels = vm(f"docker inspect --format '{{{{json .Config.Labels}}}}' {u}-pr-{PREVIEW_PR}")
                    fqdn = row.get("fqdn") or ""
                    host = fqdn.split("://", 1)[-1].split("/")[0]
                    labels = re.sub(r'"(com\.docker\.compose\.[^"]*)":"[^"]*"', r'"\1":""', labels)
                    code = vm(f"curl -s -o /dev/null -w '%{{http_code}}' -H 'Host: {host}' http://127.0.0.1/")
                    pairs = [(u, "U"), (commit or "", "SHA"), (f"{case}-ref", "NAME"), (f"{case}-rs", "NAME"), (host, "HOST")]
                    pr_file = vm(f"cat /data/coolify/applications/{u}/docker-compose-pr-{PREVIEW_PR}.yaml")
                    seen.append(dict(prod=norm(prod, pairs), pr=norm(pr, pairs), labels=norm(labels, pairs), code=code, file=norm(pr_file, pairs).replace(host, "HOST"),
                                     row={k: norm(str(row.get(k)), pairs) for k in ("pull_request_id", "docker_compose_domains", "pull_request_html_url", "domain_port_overrides", "docker_registry_image_tag", "status", "restart_count")}))
                record(f"[{case}] the preview runs beside production alike", seen[0] == seen[1] and "running" in seen[0]["prod"] and "running" in seen[0]["pr"], "\n".join(difflib.unified_diff(json.dumps(seen[0], indent=0).splitlines(), json.dumps(seen[1], indent=0).splitlines(), lineterm="")))
            ip_pairs = [(ip, "IP") for ip in ips.values()]
            if case == "rollback":
                images = [vm(f"docker ps --filter label=coolify.applicationUuid={u} --format '{{{{.Image}}}}'").strip() for u in (ur, us)]
                record(f"[{case}] both run the older commit", images[0] == f"{ur}:{ROLLBACK_COMMIT}" and images[1] == f"{us}:{ROLLBACK_COMMIT}", images)
            fr = norm(vm(f"cat /data/coolify/applications/{ur}/docker-compose.yaml"), [(ur, "U"), (f"{case}-ref", "NAME")] + ip_pairs)
            fs = norm(vm(f"cat /data/coolify/applications/{us}/docker-compose.yaml"), [(us, "U"), (f"{case}-rs", "NAME")] + ip_pairs)
            if case in STORAGES:
                # `fileStorages()->get()` has no ORDER BY: upstream's mount order is the heap order of a table
                # other writers share, and two runs of the reference differ.
                fr, fs = sorted_mounts(fr), sorted_mounts(fs)
            record(f"[{case}] docker-compose.yaml identical", fr == fs and (fr != "" or case in NOTHING_RUNS),
                   "\n".join(difflib.unified_diff(fr.splitlines(), fs.splitlines(), lineterm="")))
            envs = []
            for u in (ur, us):
                names = sorted(vm(f"docker ps -a --filter label=coolify.applicationUuid={u} --format '{{{{.Names}}}}'").split())
                per = []
                for name in names:
                    env = json.loads(vm(f"docker inspect --format '{{{{json .Config.Env}}}}' {name}") or "[]")
                    state = vm(f"docker inspect --format '{{{{.State.Status}}}}' {name}").strip()
                    def secret(x):
                        # Generated SERVICE_PASSWORD_* / SERVICE_USER_* / … values differ per application.
                        k, _, v = x.partition("=")
                        if re.match(r"^(SERVICE_(PASSWORD|USER|BASE64|REALBASE64|HEX)|[A-Z0-9_]*PASSWORD)", k):
                            return f"{k}=<{len(v)} chars>"
                        return x
                    per.append((norm(name, [(u, "U")]), state, norm(vm(f"docker inspect --format '{HOST_CONFIG}' {name}"), [(u, "U")]), sorted(secret(norm(x, [(u, "U"), (f"{case}-ref", "NAME"), (f"{case}-rs", "NAME")])) for x in env)))
                envs.append(per)
            record(f"[{case}] containers, states and environment identical", envs[0] == envs[1] and (envs[0] != [] or case in NOTHING_RUNS),
                   "\n".join(difflib.unified_diff([repr(x) for x in envs[0]], [repr(x) for x in envs[1]], lineterm="")))
            def tree(u):
                listing = vm(f"cd /data/coolify/applications/{u} && find . -printf '%y %m %U %p\\n' | sort")
                contents = vm(f"cd /data/coolify/applications/{u} && find . -type f -path './etc/*' -exec sh -c 'echo \"== $1\"; cat \"$1\"' sh {{}} \\;  | sort")
                return listing + contents
            if case in STORAGES or case in OWN_DELETE:
                tr, ts = tree(ur), tree(us)
                record(f"[{case}] configuration directories identical", tr == ts and tr != "", "\n".join(difflib.unified_diff(tr.splitlines(), ts.splitlines(), lineterm="")))
                rows = []
                for u in (ur, us):
                    rows.append(sql(f"SELECT coalesce(json_agg(json_build_object('fs', replace(fs_path, '{u}', 'U'), 'mount', mount_path, 'dir', is_directory, 'host', is_host_file, 'git', is_based_on_git, 'chmod', chmod, 'chown', chown, 'content', content IS NOT NULL AND NOT is_based_on_git) ORDER BY mount_path)::text, '') FROM local_file_volumes WHERE resource_type = 'App\\Models\\Application' AND resource_id = (SELECT id FROM applications WHERE uuid='{u}')"))
                record(f"[{case}] local_file_volumes rows alike", rows[0] == rows[1], rows)
            if case in STORAGES:
                inside = []
                for u in (ur, us):
                    name = vm(f"docker ps --filter label=coolify.applicationUuid={u} --format '{{{{.Names}}}}'").split()
                    inside.append(vm(f"docker exec {name[0]} sh -c 'ls -la /etc/diff | cut -d\" \" -f1; cat /etc/diff/app.conf'") if name else "no container")
                record(f"[{case}] the container sees the same files", inside[0] == inside[1], inside)

            def own_delete():
                if case not in OWN_DELETE:
                    return
                for side, u in (("ref", ur), ("rs", us)):
                    c, r = call(side, "DELETE", f"/api/v1/applications/{u}?delete_volumes=true", tok="acme_owner_root")
                    record(f"[{case}] {side} accepts the delete", c == 200, f"{c} {r}")
                time.sleep(5)
                gone = []
                for u in (ur, us):
                    for _ in range(60):
                        left = (sql(f"SELECT count(*) FROM applications WHERE uuid='{u}'"),
                                vm(f"ls -A /data/coolify/applications/{u} 2>/dev/null | wc -l").strip(),
                                vm(f"docker ps -aq --filter label=coolify.applicationUuid={u} | wc -l").strip())
                        if left == ("0", "0", "0"):
                            break
                        time.sleep(3)
                    gone.append(left)
                record(f"[{case}] delete removes the row, the files and the containers", gone[0] == gone[1] == ("0", "0", "0"), gone)
            if spec.get("build_pack") == "dockercompose":
                for col in ("docker_compose", "docker_compose_domains", "fqdn"):
                    vr = json.loads(sql(f"SELECT to_json({col})::text FROM applications WHERE uuid='{ur}'") or "null")
                    vs = json.loads(sql(f"SELECT to_json({col})::text FROM applications WHERE uuid='{us}'") or "null")
                    vr = norm(json.dumps(vr), [(ur, "U"), (f"{case}-ref", "NAME")])
                    vs = norm(json.dumps(vs), [(us, "U"), (f"{case}-rs", "NAME")])
                    record(f"[{case}] applications.{col} alike", vr == vs, f"{vr[:800]}\n  vs {vs[:800]}")
                own_delete()
                continue
            codes, authed = [], []
            for u in (ur, us):
                fqdn = json.loads(sql(f"SELECT to_json(fqdn)::text FROM applications WHERE uuid='{u}'") or "null") or ""
                host = fqdn.split("://", 1)[-1].split("/")[0]
                codes.append(vm(f"curl -s -o /dev/null -w '%{{http_code}}' -H 'Host: {host}' http://127.0.0.1/"))
                if spec.get("is_http_basic_auth_enabled"):
                    authed.append(vm(f"curl -s -o /dev/null -w '%{{http_code}}' -u {spec['http_basic_auth_username']}:{spec['http_basic_auth_password']} -H 'Host: {host}' http://127.0.0.1/"))
            if authed:
                record(f"[{case}] the proxy asks for the password", codes[0] == codes[1] == "401", codes)
                record(f"[{case}] the proxy accepts the password", authed[0] == authed[1] == "200", authed)
            else:
                record(f"[{case}] both answer through the proxy", codes[0] == codes[1] == "200", codes)
            if case not in STATUS_CASES:
                own_delete()
                continue
            time.sleep(70)  # both status pollers run at least once
            ar = json.loads(sql(f"SELECT row_to_json(x)::text FROM applications x WHERE uuid='{ur}'"))
            as_ = json.loads(sql(f"SELECT row_to_json(x)::text FROM applications x WHERE uuid='{us}'"))
            keys = ["status", "custom_healthcheck_found", "health_check_interval", "health_check_timeout", "health_check_retries",
                    "health_check_start_period", "health_check_enabled", "config_hash"]
            record(f"[{case}] application health and status columns alike",
                   all(ar[k] == as_[k] for k in keys if k != "config_hash"), {k: (ar[k], as_[k]) for k in keys if k != "config_hash" and ar[k] != as_[k]})
    finally:
        for u in created:
            call("ref", "DELETE", f"/api/v1/applications/{u}?delete_volumes=true", tok="acme_owner_root")
        time.sleep(5)
        # DeleteResourceJob reads the project: without it the job skips deleteConfigurations() and the directory stays.
        for _ in range(180):
            if not created or sql("SELECT count(*) FROM applications WHERE uuid IN ({})".format(", ".join(f"'{u}'" for u in created))) == "0":
                break
            time.sleep(5)
        call("ref", "DELETE", f"/api/v1/projects/{project}", tok="acme_owner_root")
        vm(f"rm -f {seccomp}")
        if key_uuid:
            time.sleep(10)
            call("ref", "DELETE", f"/api/v1/security/keys/{key_uuid}", tok="acme_owner_root")
    passed = sum(results)
    print(f"\n{passed}/{len(results)} checks identical")
    sys.exit(0 if passed == len(results) else 1)


if __name__ == "__main__":
    main()
