#!/usr/bin/env python3
"""Moves a real Coolify install to coolify-rs and back, the way docs/MIGRATING.md tells users to.

    migrate.py <private work folder> <coolify-rs image tag> [stage ...]

One throwaway OrbStack VM: upstream's own installer at the release coolify-rs matches (UP_VERSION, default 4.6.0, from
.upstream/snapshot-<version>), a deployment on it, the dry-run check, the coolify-rs installer over it, checks and a
deployment on coolify-rs, then upstream's installer again to go back. The image is coolify-rs/coolify-rs:<tag> in the
local Docker. Stages: clean up user seed dryrun migrate after back delete. Without stages, all run and the VM is deleted at
the end. The work folder receives the throwaway root password and the image file; `delete` removes them.
"""
import os
import secrets
import shutil
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
VM, PORT = "coolify-rs-agent-install-up", "28093"
UP_VERSION = os.environ.get("UP_VERSION", "4.6.0")
work, tag = Path(sys.argv[1]), sys.argv[2]
stages = sys.argv[3:]
failures = []
work.mkdir(parents=True, exist_ok=True)
PASSWORD = work / "migrate-root-pass.txt"
IMAGE_TAR = work / f"image-{tag}.tar"
PSQL = ["docker", "exec", "coolify-db", "psql", "-U", "coolify", "-d", "coolify", "-Atc"]
COUNTS = (
    "SELECT (SELECT count(*) FROM migrations), (SELECT count(*) FROM users), (SELECT count(*) FROM teams), "
    "(SELECT count(*) FROM servers), (SELECT count(*) FROM private_keys), (SELECT count(*) FROM projects), "
    "(SELECT count(*) FROM applications), (SELECT count(*) FROM application_deployment_queues), "
    "(SELECT count(*) FROM environment_variables), (SELECT count(*) FROM shared_environment_variables)"
)
state = {}


def run(*args, timeout=900, env=None, stdin=None):
    print("$", " ".join(str(a) for a in args)[:140], flush=True)
    p = subprocess.run([str(a) for a in args], capture_output=True, text=True, timeout=timeout, env=env, input=stdin)
    return p.returncode, (p.stdout + p.stderr)


def vm(*args, timeout=300, stdin=None):
    return run("orb", "-m", VM, "-u", "root", *args, timeout=timeout, stdin=stdin)


def check(label, ok, detail=""):
    print(("ok   " if ok else "FAIL ") + label + (f"  {detail}" if detail else ""), flush=True)
    if not ok:
        failures.append(label)


def show(label, code, output, tail=20):
    print("\n".join(l[:300] for l in output.strip().splitlines()[-tail:]), flush=True)
    check(label, code == 0, f"exit {code}")


def counts():
    return vm(*PSQL, COUNTS)[1].strip()


def secrets_fingerprint():
    return vm("bash", HERE / "vm_secrets.sh")[1].strip()


def app_containers():
    """Names and start times of the deployed resources' containers: they must survive the switch."""
    _, names = vm("docker", "ps", "--filter", "label=coolify.managed=true", "--format", "{{.Names}}")
    out = []
    for name in sorted(names.split()):
        _, started = vm("docker", "inspect", name, "--format", "{{.State.StartedAt}}")
        out.append(f"{name} {started.strip()}")
    return out


def wait_healthy(image_fragment):
    for _ in range(90):
        _, out = vm("docker", "inspect", "coolify", "--format", "{{.Config.Image}} {{.State.Health.Status}}")
        if image_fragment in out and out.strip().endswith("healthy"):
            return out.strip()
        time.sleep(5)
    return out.strip()


def stage_clean():
    run("orb", "delete", "-f", VM)


def stage_up():
    show("prepare VM", *run(HERE / "vm_prepare.sh", VM))
    env = dict(os.environ, UP_SNAPSHOT=str(ROOT / f".upstream/snapshot-{UP_VERSION}"), UP_VERSION=UP_VERSION)
    show(f"Coolify {UP_VERSION} installer", *run(HERE / "up_install.sh", timeout=1500, env=env), tail=10)
    status = wait_healthy(f"coolify:{UP_VERSION}")
    check("Coolify is healthy", "healthy" in status, status)


def stage_user():
    # The first user, as RootUserSeeder makes it. Its rfc,dns email and breached-password rules need the network
    # the VM blocks on purpose, so the seeder's own steps run directly.
    PASSWORD.write_text(secrets.token_urlsafe(24))
    # The password goes in on stdin, so it is never in a command line or the log. User 0 joins team 0 by itself.
    code, out = vm("docker", "exec", "-i", "coolify", "php", "artisan", "tinker", "--execute",
                   '$p = trim(file_get_contents("php://stdin")); '
                   '$u = (new App\\Models\\User)->forceFill(["id" => 0, "name" => "Root User", '
                   '"email" => "root@migrate.test", "password" => Illuminate\\Support\\Facades\\Hash::make($p)]); '
                   '$u->save(); echo "user " . $u->id . " teams " . $u->teams()->count();', stdin=PASSWORD.read_text())
    check("first user", "user 0 teams 1" in out, out.strip()[-200:])


# Names upstream later renamed or deleted. A long-lived Coolify install carries these rows next to the current
# names (seen on a real 4.6.0 instance), and coolify-rs must still take the database.
HISTORICAL_MIGRATIONS = [
    "2018_08_08_100000_create_telescope_entries_table",
    "2025_10_10_120000_create_cloud_init_scripts_table",
    "2025_10_10_120000_create_webhook_notification_settings_table",
    "2025_10_10_120001_populate_webhook_notification_settings_for_existing_teams",
]


def stage_seed():
    # From 4.6.0 on, Coolify on this offline VM marks the proxy "starting" after validation but never starts it, so
    # start it the way the dashboard's Start Proxy button does. The migration must then leave it running.
    code, out = vm("docker", "exec", "coolify", "php", "artisan", "tinker", "--execute",
                   'App\\Actions\\Proxy\\StartProxy::run(App\\Models\\Server::find(0), async: false); echo "started";')
    check("Coolify's proxy", "started" in out, out.strip()[-200:])
    show("deployments on Coolify", *run("python3", HERE / "deploy_check.py", VM, PORT, timeout=1500), tail=14)
    values = ", ".join(f"('{m}', (SELECT max(batch) FROM migrations))" for m in HISTORICAL_MIGRATIONS)
    code, out = vm("docker", "exec", "coolify-db", "psql", "-U", "coolify", "-d", "coolify", "-Atc",
                   f"INSERT INTO migrations (migration, batch) VALUES {values}")
    check("old migration names of a long-lived install", code == 0, out.strip())


def stage_dryrun():
    IMAGE_TAR.parent.mkdir(parents=True, exist_ok=True)
    show("save coolify-rs image", *run("docker", "save", f"coolify-rs/coolify-rs:{tag}", "-o", IMAGE_TAR))
    show("load coolify-rs image", *vm("bash", HERE / "vm_load_image.sh", IMAGE_TAR))
    # The documented dry run, word for word apart from the registry.
    code, out = vm("bash", "-c",
                   "docker run --rm --network coolify --env-file /data/coolify/source/$(printf %s .env) "
                   f"-e DB_HOST=coolify-db coolify-rs/coolify-rs:{tag} check-schema")
    print(out.strip()[-600:])
    check("dry run: coolify-rs can take the database", code == 0 and '"read_only_reason": null' in out)
    _, image = vm("docker", "inspect", "coolify", "--format", "{{.Config.Image}}")
    check("dry run changed nothing", f"coolify:{UP_VERSION}" in image, image.strip())


def stage_migrate():
    state["counts"] = counts()
    state["secrets"] = secrets_fingerprint()
    state["apps"] = app_containers()
    print("rows before:", state["counts"])
    show("publish coolify-rs installer", *vm("bash", HERE / "rs_cdn.sh", ROOT / "scripts/install", tag))
    show("coolify-rs installer over Coolify", *vm("bash", HERE / "rs_run_installer.sh", IMAGE_TAR, timeout=1500), tail=12)


def stage_after():
    status = wait_healthy(f"coolify-rs:{tag}")
    check("coolify-rs is healthy", "healthy" in status, status)
    after = counts()
    check("rows unchanged", after == state.get("counts", after), f"{state.get('counts')} -> {after}")
    check("secrets, key and authorized key unchanged", secrets_fingerprint() == state.get("secrets", ""))
    apps = app_containers()
    check("deployed containers kept running", apps == state.get("apps", apps) and len(apps) > 0, f"{len(apps)} containers")
    _, out = vm("docker", "exec", "coolify", "coolify-rs", "check-schema")
    check("writable", '"read_only_reason": null' in out, out.strip()[-200:])
    _, names = vm("docker", "ps", "-a", "--format", "{{.Names}}")
    check("no coolify-realtime container", "coolify-realtime" not in names.split(), " ".join(sorted(names.split())))
    _, version = vm("bash", "-c", "docker inspect coolify --format '{{range .Config.Env}}{{println .}}{{end}}' | grep ^COOLIFY_VERSION=")
    check("reports the release version", version.strip() == f"COOLIFY_VERSION={tag}", version.strip())
    show("deployments on coolify-rs", *run("python3", HERE / "deploy_check.py", VM, PORT, timeout=1500), tail=14)
    code, out = vm("docker", "exec", "coolify", "coolify-rs", "check-portable")
    check("Coolify can take the database back", code == 0, out.strip()[-200:])
    state["counts_rs"] = counts()


def stage_back():
    env = dict(os.environ, UP_SNAPSHOT=str(ROOT / f".upstream/snapshot-{UP_VERSION}"), UP_VERSION=UP_VERSION)
    show(f"Coolify {UP_VERSION} installer again", *run(HERE / "up_install.sh", timeout=1500, env=env), tail=10)
    status = wait_healthy(f"coolify:{UP_VERSION}")
    check("Coolify is back and healthy", "healthy" in status, status)
    after = counts()
    check("rows kept", after == state.get("counts_rs", after), f"{state.get('counts_rs')} -> {after}")
    _, pending = vm("docker", "exec", "coolify", "php", "artisan", "migrate:status", "--pending", "--no-ansi")
    check("no pending migrations", "Pending" not in pending, pending.strip()[-200:])
    _, out = vm("docker", "exec", "coolify", "php", "artisan", "tinker", "--execute",
                '$s = App\\Models\\Server::find(0); '
                'echo "ssh: " . trim(instant_remote_process(["echo reached"], $s)) . PHP_EOL; '
                'echo "key: " . (str_contains($s->privateKey->private_key, "PRIVATE KEY") ? "yes" : "no") . PHP_EOL;')
    check("Coolify reaches the server with the same key", "ssh: reached" in out and "key: yes" in out, out.strip()[-200:])


def stage_delete():
    run("orb", "delete", "-f", VM)
    PASSWORD.unlink(missing_ok=True)
    IMAGE_TAR.unlink(missing_ok=True)


ALL = ["clean", "up", "user", "seed", "dryrun", "migrate", "after", "back", "delete"]
NEEDS_TUNNEL = {"seed", "after"}
tunnel = None
try:
    for name in stages or ALL:
        print(f"\n===== {name}", flush=True)
        if name in NEEDS_TUNNEL and tunnel is None:
            tunnel = subprocess.Popen(["python3", str(HERE / "orb_tunnel.py"), VM, PORT, "8000"])
            time.sleep(2)
        globals()["stage_" + name]()
except Exception as e:  # noqa: BLE001
    failures.append(f"{type(e).__name__}: {e}")
finally:
    if tunnel:
        tunnel.kill()
    if not stages:
        stage_delete()
print("\nRESULT", "all stages passed" if not failures else f"failures: {failures}", flush=True)
sys.exit(1 if failures else 0)
