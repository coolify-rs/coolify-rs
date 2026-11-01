#!/usr/bin/env python3
"""The clean install comparison of Milestone 7, from two fresh VMs to deleting them.

    run.py <private work folder> <image tag> <newer image tag> [stage ...]

Images are coolify-rs/coolify-rs:<tag> in the local Docker. Without stages, all run in order, and both VMs
are deleted at the end even after a failure. Stages: clean-vms up rs diff-clean refuse firstrun diff-first
rerun deploy abort upgrade adopt delete. The work folder receives raw dumps with the throwaway installs'
secrets; `delete` removes the dumps too.
"""
import shutil
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
UP, RS = "coolify-rs-agent-install-up", "coolify-rs-agent-install-rs"
UP_PORT, RS_PORT = "28093", "28094"
work, tag, newer = Path(sys.argv[1]), sys.argv[2], sys.argv[3]
stages = sys.argv[4:]
failures = []
work.mkdir(parents=True, exist_ok=True)
PASSWORD = work / "firstrun-pass.txt"


def run(*args, timeout=900, cwd=None):
    print("$", " ".join(str(a) for a in args)[:140], flush=True)
    p = subprocess.run([str(a) for a in args], capture_output=True, text=True, timeout=timeout, cwd=cwd)
    return p.returncode, (p.stdout + p.stderr)


def show(label, code, output, tail=30):
    lines = output.strip().splitlines()
    print("\n".join(l[:300] for l in lines[-tail:]), flush=True)
    print(f"[{label}: exit {code}]", flush=True)
    if code != 0:
        failures.append(label)


def on(vm, script, *args, timeout=900):
    return run("orb", "-m", vm, "-u", "root", "bash", HERE / script, *args, timeout=timeout)


def wait_settled(vm):
    """Proxy and Sentinel start in the background after the installer returns."""
    for _ in range(60):
        _, out = run("orb", "-m", vm, "-u", "root", "docker", "ps", "--format", "{{.Names}} {{.Status}}")
        if all(any(l.startswith(n + " ") and "starting" not in l for l in out.splitlines()) for n in ["coolify-proxy", "coolify-proxy-error-pages", "coolify-sentinel", "coolify"]):
            _, proxy = run("orb", "-m", vm, "-u", "root", "docker", "exec", "coolify-db", "psql", "-U", "coolify", "-d", "coolify", "-Atc", "SELECT proxy->>'status' FROM servers")
            if proxy.strip() == "running":
                return
        time.sleep(5)
    failures.append(f"{vm} did not settle")


def diff(up_dir, rs_dir, label):
    for vm, folder in ((UP, up_dir), (RS, rs_dir)):
        code, out = on(vm, "vm_dump.sh", work / folder)
        if code:
            show(f"dump {vm}", code, out)
    show(label, *run("python3", "-I", HERE.parent / "install_diff.py", work / up_dir, work / rs_dir), tail=80)


def stage_clean_vms():
    run("orb", "delete", "-f", UP)
    run("orb", "delete", "-f", RS)


def stage_up():
    show("prepare upstream VM", *run(HERE / "vm_prepare.sh", UP))
    show("upstream installer", *run(HERE / "up_install.sh", timeout=1500), tail=14)
    wait_settled(UP)


def stage_rs():
    show("prepare coolify-rs VM", *run(HERE / "vm_prepare.sh", RS))
    env_cmd = ["/usr/bin/env", f"INSTALL_SCRATCH={work / 'images'}", HERE / "rs_install.sh", tag]
    show("coolify-rs installer", *run(*env_cmd, timeout=1500), tail=14)
    wait_settled(RS)


def stage_diff_clean():
    diff("up-clean", "rs-clean", "clean installs compared")


def stage_refuse():
    # Against this VM's own database, which already has tables.
    code, out = run("orb", "-m", RS, "-u", "root", "docker", "exec", "coolify", "/usr/local/bin/coolify-rs", "create-schema")
    refused = code != 0 and "refusing to create the schema" in out
    print(("ok   " if refused else "FAIL ") + f"create-schema refuses the installed database (exit {code})")
    if not refused:
        failures.append("refusal")
    code, _ = run("orb", "-m", RS, "-u", "root", "docker", "exec", "coolify", "/usr/local/bin/coolify-rs", "healthcheck")
    ok = code == 0
    code2, _ = run("orb", "-m", RS, "-u", "root", "docker", "exec", "-e", "COOLIFY_RS_LISTEN=0.0.0.0:1", "coolify", "/usr/local/bin/coolify-rs", "healthcheck")
    ok2 = code2 != 0
    print(("ok   " if ok and ok2 else "FAIL ") + f"healthcheck: server up exit {code}, nothing listening exit {code2}")
    if not (ok and ok2):
        failures.append("healthcheck")


def stage_firstrun():
    show("first-run screens", *run("node", "install_firstrun.mjs", UP_PORT, RS_PORT, PASSWORD, cwd=ROOT / "compat/ui"), tail=20)


def stage_diff_first():
    time.sleep(20)
    diff("up-first", "rs-first", "after the first user")


def stage_rerun():
    _, before = on(RS, "vm_secrets.sh")
    show("second install run", *on(RS, "vm_rerun.sh"), tail=6)
    _, after = on(RS, "vm_secrets.sh")
    same = before == after and before.strip() != ""
    print(("ok   " if same else "FAIL ") + "secrets, key and authorized_keys entry unchanged by the rerun")
    if not same:
        failures.append("rerun changed secrets")


def stage_deploy():
    show("deployments", *run("python3", HERE / "deploy_check.py", RS, RS_PORT))


def stage_abort():
    code, out = on(RS, "vm_backup_abort.sh")
    ok = code == 0 and "exit=1" in out and "status=error|The database backup failed" in out and "container unchanged" in out
    print(out.strip()[:400])
    print(("ok   " if ok else "FAIL ") + "a failed backup stops the upgrade")
    if not ok:
        failures.append("backup abort")


def stage_upgrade():
    tar = work / "images" / f"image-{newer}.tar"
    tar.parent.mkdir(parents=True, exist_ok=True)
    show("save newer image", *run("docker", "save", f"coolify-rs/coolify-rs:{newer}", "-o", tar))
    show("load newer image", *on(RS, "vm_load_image.sh", tar))
    show("publish newer version", *on(RS, "rs_cdn.sh", ROOT / "scripts/install", newer))
    _, before = on(RS, "vm_secrets.sh")
    # Rows of the first version stay; a restart signs users out, so the script logs in first.
    show("upgrade from the UI", *run("node", "install_upgrade.mjs", RS_PORT, PASSWORD, newer, cwd=ROOT / "compat/ui", timeout=400))
    time.sleep(15)
    show("after the upgrade", *on(RS, "vm_after_upgrade.sh"))
    _, after = on(RS, "vm_secrets.sh")
    same = before == after and before.strip() != ""
    print(("ok   " if same else "FAIL ") + "secrets unchanged by the upgrade")
    if not same:
        failures.append("upgrade changed secrets")
    with urllib.request.urlopen(f"http://127.0.0.1:{RS_PORT}/api/health", timeout=20) as r:
        print("health", r.status, flush=True)


def stage_adopt():
    stage = work / "upstream-compose"
    stage.mkdir(exist_ok=True)
    for name in ("docker-compose.yml", "docker-compose.prod.yml"):
        shutil.copy(ROOT / ".upstream/snapshot" / name, stage / name)
    show("upstream adopts the database", *on(RS, "vm_adopt_upstream.sh", stage, timeout=900))
    show("upstream UI on that database", *run("node", "install_adopted.mjs", RS_PORT, PASSWORD, cwd=ROOT / "compat/ui", timeout=200))


def stage_delete():
    run("orb", "delete", "-f", UP)
    run("orb", "delete", "-f", RS)
    for folder in ("up-clean", "rs-clean", "up-first", "rs-first", "images", "upstream-compose"):
        shutil.rmtree(work / folder, ignore_errors=True)
    PASSWORD.unlink(missing_ok=True)


ALL = ["clean-vms", "up", "rs", "diff-clean", "refuse", "firstrun", "diff-first", "rerun", "deploy", "abort", "upgrade", "adopt", "delete"]
NEEDS_TUNNEL = {"firstrun", "deploy", "upgrade", "adopt"}
tunnels = []
try:
    for name in stages or ALL:
        print(f"\n===== {name}", flush=True)
        if name in NEEDS_TUNNEL and not tunnels:
            tunnels = [
                subprocess.Popen(["python3", str(HERE / "orb_tunnel.py"), UP, UP_PORT, "8000"]),
                subprocess.Popen(["python3", str(HERE / "orb_tunnel.py"), RS, RS_PORT, "8000"]),
            ]
            time.sleep(2)
        globals()["stage_" + name.replace("-", "_")]()
except Exception as e:  # noqa: BLE001
    failures.append(f"{type(e).__name__}: {e}")
finally:
    for t in tunnels:
        t.kill()
    if not stages:
        stage_delete()
print("\nRESULT", "all stages passed" if not failures else f"failures: {failures}", flush=True)
sys.exit(1 if failures else 0)
