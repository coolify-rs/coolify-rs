#!/usr/bin/env python3
"""Differential test of the notification messages.

The same inputs are rendered by the reference (tinker, compat/diff/notifications_ref.php, nothing
saved, Http::fake) and by coolify-rs (examples/notifications_render.rs). Compared byte for byte:
subject, mail HTML and the request body of Discord, Slack, Mattermost, Telegram, Pushover and
webhook, plus the event name of via(). Only the `<t:...:R>` timestamp of Discord is masked.

Usage: python3 compat/diff/notifications_diff.py [-v]
"""

import json
import os
import re
import subprocess
import sys
import tempfile

CONTAINER = "coolify-rs-reference-coolify-1"
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

# Strings that exercise escaping, markdown and non-ASCII handling.
NASTY = "Tom's <b>\"app\"</b> & co_1 *x* [y](z) #h `c` | é 日本 \\ /p"
MULTI = "line one\nline two\n\nparagraph <two> & 'three'\n    indented\n- item\n- item2\n\n1. n\n"
SERVER = {"id": 999000, "name": "web-1", "uuid": "srv-uuid"}
SERVER_NASTY = {"id": 999000, "name": NASTY, "uuid": "srv-uuid"}


def deployment(**over):
    base = {
        "application_name": "my-app",
        "application_uuid": "app-uuid",
        "deployment_uuid": "dep-uuid",
        "project_name": "Proj",
        "project_uuid": "p-uuid",
        "environment_name": "production",
        "environment_uuid": "e-uuid",
        "fqdn": "https://a.example.com,https://b.example.com",
        "preview": None,
        "commit": None,
    }
    base.update(over)
    return base


def backup(**over):
    base = {
        "name": "pg-main",
        "uuid": "db-uuid",
        "project_uuid": "p-uuid",
        "environment_uuid": "e-uuid",
        "database_name": "postgres",
        "frequency": "0 0 * * *",
    }
    base.update(over)
    return base


def task(**over):
    base = {
        "name": "cron1",
        "uuid": "t-uuid",
        "output": "ok\ndone",
        "resource": {"is_application": True, "uuid": "app-uuid", "project_uuid": "p-uuid", "environment_uuid": "e-uuid"},
    }
    base.update(over)
    return base


def restart(**over):
    base = {
        "kind": "application",
        "resource_name": "my-app",
        "resource_uuid": "r-uuid",
        "url_uuid": "r-uuid",
        "project_name": "Proj",
        "project_uuid": "p-uuid",
        "environment_name": "production",
        "environment_uuid": "e-uuid",
        "fqdn": "https://a.example.com,https://b.example.com",
        "restart_count": 10,
        "max_restart_count": 10,
    }
    base.update(over)
    return base


def volume(**over):
    base = {"resource_name": "my-app", "target": "volume data", "frequency": "daily", "url": "https://c.example/res"}
    base.update(over)
    return base


UPDATES = [
    {"package": "docker-ce", "current_version": "1.0", "new_version": "1.1", "architecture": "amd64", "repository": "docker", "security": True, "risk": "docker"},
    {"package": "curl", "current_version": "7", "new_version": "8", "architecture": None, "repository": None},
    {"package": "systemd", "current_version": "1", "new_version": "2", "risk": "reboot"},
    {"package": "openssl", "current_version": "3", "new_version": "3.1", "risk": "core"},
    {"package": "linux", "current_version": "6", "new_version": "6.1", "risk": "reboot"},
    {"package": "vim<x>", "current_version": "9", "new_version": "9.1"},
]
SSL = [
    {"name": "pg-1", "uuid": "u1", "project_uuid": "p", "environment_uuid": "e"},
    {"name": "my <db>", "uuid": "u2", "project_uuid": "p", "environment_uuid": "e"},
]
COMMIT = {"sha": "abcdef1234", "message": "fix: a_b\n\nsecond [x]", "url": "https://g.example/o/r/commit/abcdef1234", "branch": "feat/x_y"}


def n(type_, **fields):
    return {"type": type_, **fields}


NOTIFICATIONS = {
    "deploy-success": n("DeploymentSuccess", **deployment()),
    "deploy-success-nofqdn": n("DeploymentSuccess", **deployment(fqdn=None)),
    "deploy-success-localhost": n("DeploymentSuccess", **deployment(fqdn="http://localhost:3000/x")),
    "deploy-success-preview": n("DeploymentSuccess", **deployment(preview={"pull_request_id": 42, "fqdn": "https://pr.example"})),
    "deploy-success-preview-nofqdn": n("DeploymentSuccess", **deployment(preview={"pull_request_id": 42, "fqdn": None})),
    "deploy-success-nasty": n("DeploymentSuccess", **deployment(application_name=NASTY, project_name=NASTY, environment_name=NASTY)),
    "deploy-success-commit": n("DeploymentSuccess", **deployment(commit=COMMIT)),
    "deploy-failed": n("DeploymentFailed", **deployment()),
    "deploy-failed-nofqdn": n("DeploymentFailed", **deployment(fqdn=None)),
    "deploy-failed-preview": n("DeploymentFailed", **deployment(preview={"pull_request_id": 7, "fqdn": "https://pr.example"})),
    "deploy-failed-preview-nofqdn": n("DeploymentFailed", **deployment(fqdn=None, preview={"pull_request_id": 7, "fqdn": None})),
    "deploy-failed-commit": n("DeploymentFailed", **deployment(commit={"sha": None, "message": "only message", "url": None, "branch": None})),
    "status-changed": n("StatusChanged", name="my-app", uuid="u", project_name="P", project_uuid="pu", environment_name="prod", environment_uuid="eu", fqdn="https://a.example,https://b.example"),
    "restart-application": n("RestartLimitReached", **restart()),
    "restart-preview": n("RestartLimitReached", **restart(kind="preview", resource_name="my-app PR #3", url_uuid="app-uuid", fqdn=None)),
    "restart-service": n("RestartLimitReached", **restart(kind="service", resource_name="svc", url_uuid="svc-uuid")),
    "restart-database": n("RestartLimitReached", **restart(kind="database", resource_name="pg", fqdn=None)),
    "container-restarted-proxy": n("ContainerRestarted", name="coolify-proxy", server=SERVER, url=None),
    "container-restarted-url": n("ContainerRestarted", name=NASTY, server=SERVER_NASTY, url="https://c.example/x?a=1&b=2"),
    "backup-success": n("BackupSuccess", **backup()),
    "backup-success-nodb": n("BackupSuccess", **backup(database_name=None)),
    "backup-failed": n("BackupFailed", backup=backup(), output="pg_dump: error"),
    "backup-failed-multi": n("BackupFailed", backup=backup(name=NASTY), output=MULTI),
    "backup-failed-table": n("BackupFailed", backup=backup(), output="a | b\n--- | ---\nc | d\n"),
    "backup-missing": n("BackupMissing", database_name="pg <main>", backup_uuid="b-uuid", days=3, last_execution_at="2026-10-01 12:00:00"),
    "backup-missing-never": n("BackupMissing", database_name="pg", backup_uuid="b-uuid", days=1, last_execution_at=None),
    "backup-s3": n("BackupSuccessWithS3Warning", backup=backup(), s3_error="connection refused", s3_uuid="s3-uuid"),
    "backup-s3-none": n("BackupSuccessWithS3Warning", backup=backup(database_name=None), s3_error=MULTI, s3_uuid=None),
    "general": n("General", message="hello <b>", success=True),
    "general-fail": n("General", message=MULTI, success=False),
    "task-success": n("TaskSuccess", **task()),
    "task-success-service": n("TaskSuccess", **task(resource={"is_application": False, "uuid": "svc", "project_uuid": "p", "environment_uuid": "e"})),
    "task-success-none": n("TaskSuccess", **task(resource=None)),
    "task-failed": n("TaskFailed", **task(output=MULTI)),
    "task-failed-empty": n("TaskFailed", **task(output="", resource=None)),
    "cleanup-success": n("DockerCleanupSuccess", server=SERVER, message="Saved 10% disk space."),
    "cleanup-success-nasty": n("DockerCleanupSuccess", server=SERVER_NASTY, message=MULTI),
    "cleanup-failed": n("DockerCleanupFailed", server=SERVER, message="Docker cleanup job failed with the following error: x"),
    "force-disabled": n("ForceDisabled", server=SERVER_NASTY),
    "force-enabled": n("ForceEnabled", server=SERVER_NASTY),
    "hetzner": n("HetznerDeletionFailed", hetzner_server_id=123, error=MULTI),
    "disk": n("HighDiskUsage", server=SERVER, disk_usage=91, threshold=80),
    "reachable": n("Reachable", server=SERVER_NASTY),
    "unreachable": n("Unreachable", server=SERVER_NASTY),
    "patch-error": n("ServerPatchCheck", server=SERVER, patch_data={"error": "apt failed <x>", "osId": "ubuntu", "package_manager": "apt"}),
    "patch-error-default": n("ServerPatchCheck", server=SERVER, patch_data={"error": "e"}),
    "patch-updates": n("ServerPatchCheck", server=SERVER, patch_data={"total_updates": 6, "updates": UPDATES, "osId": "debian", "package_manager": "apt", "security_updates": 2, "reboot_required": True}),
    "patch-two": n("ServerPatchCheck", server=SERVER, patch_data={"total_updates": 2, "updates": UPDATES[1:2] + UPDATES[5:], "osId": "arch", "package_manager": "pacman", "security_updates": None, "reboot_required": False}),
    "patch-zero": n("ServerPatchCheck", server=SERVER_NASTY, patch_data={"total_updates": 0, "updates": [], "osId": "fedora", "package_manager": "dnf"}),
    "patch-empty": n("ServerPatchCheck", server=SERVER, patch_data={}),
    "traefik-patch": n("TraefikVersionOutdated", servers=[{"name": "a", "uuid": "ua", "current": "3.5.0", "latest": "3.5.1", "update_type": "patch_update"}]),
    "traefik-mixed": n("TraefikVersionOutdated", servers=[
        {"name": "a", "uuid": "ua", "current": "v3.5.0", "latest": "v3.5.1", "update_type": "patch_update", "newer_branch_target": "v3.6", "newer_branch_latest": "3.6.2"},
        {"name": NASTY, "uuid": "ub", "current": "3.4.0", "latest": "3.4.9", "update_type": "minor_upgrade", "upgrade_target": "3.6"},
        {"name": "c", "uuid": "uc"},
    ]),
    "volume-success": n("VolumeBackupSuccess", backup=volume(), warning=None),
    "volume-success-warn": n("VolumeBackupSuccess", backup=volume(url=None), warning="partial <upload>"),
    "volume-success-unknown": n("VolumeBackupSuccess", backup=volume(resource_name=None), warning=None),
    "volume-failed": n("VolumeBackupFailed", backup=volume(), output=MULTI),
    "volume-failed-nourl": n("VolumeBackupFailed", backup=volume(url=None), output="x"),
    "volume-missing": n("VolumeBackupMissing", backup=volume(), backup_uuid="b", days=2, last_execution_at="2026-10-01 12:00:00"),
    "volume-missing-never": n("VolumeBackupMissing", backup=volume(url=None), backup_uuid="b", days=2, last_execution_at=None),
    "volume-recovery-s3": n("VolumeBackupRecoveryFailed", backup=volume(), execution_uuid="x", recovery_error="s3_auth", containers_stopped=True),
    "volume-recovery-bucket": n("VolumeBackupRecoveryFailed", backup=volume(url=None), execution_uuid="x", recovery_error="s3_bucket", containers_stopped=False),
    "volume-recovery-remote": n("VolumeBackupRecoveryFailed", backup=volume(), execution_uuid="x", recovery_error="remote_command", containers_stopped=False),
    "volume-recovery-none": n("VolumeBackupRecoveryFailed", backup=None, execution_uuid="x", recovery_error=None, containers_stopped=False),
    "token": n("ApiTokenExpiring", token_name="ci <token>", expires_at="2026-11-01 10:00:00"),
    "token-noexpiry": n("ApiTokenExpiring", token_name="ci", expires_at=None),
    "ssl": n("SslExpiration", resources=SSL),
    "ssl-empty": n("SslExpiration", resources=[]),
    "test": n("Test", emails=None, channel="discord", ping=False),
    "test-ping": n("Test", emails="a@b.c", channel=None, ping=True),
}

MAILS = {
    "mail-reset": {"type": "ResetPassword", "token": "tok/en+1", "email": "a+b@c.d"},
    "mail-invite": {"type": "TeamInvitation", "team": "Team <1> & 'co'", "invitation_link": "https://c.example/invitations/abc?a=1&b=2"},
    "mail-invitation-class": {"type": "InvitationLink", "team": "Team", "invitation_link": "https://c.example/i/x"},
    "mail-email-change": {"type": "EmailChangeVerification", "new_email": "new+x@c.d", "verification_code": "123456"},
    "mail-test": {"type": "Test"},
    "mail-channel-disabled": {"type": "ChannelDisabled", "channel": "Discord", "failures": 3, "reason": "Discord returned HTTP 404 (Not Found).", "url": "https://c.example/notifications/discord"},
}

COMMITS = {
    "commit-github": {"commit": " abcdef1234567 ", "commit_message": "  fix\r\nmore  ", "git_branch": "main", "is_preview": False, "html_url": "https://github.com", "git_repository": "o/r"},
    "commit-bitbucket": {"commit": "ABCDEF1", "commit_message": None, "git_branch": "main", "is_preview": False, "html_url": "https://bitbucket.org", "git_repository": "o/r"},
    "commit-public": {"commit": "abcdef1", "commit_message": "m", "git_branch": "dev", "is_preview": False, "html_url": None, "git_repository": "https://u:p@gitlab.example/o/r.git"},
    "commit-scp": {"commit": "abcdef1", "commit_message": "m", "git_branch": "dev", "is_preview": True, "html_url": None, "git_repository": "git@github.com:o/r.git"},
    "commit-ssh": {"commit": "abcdef1", "commit_message": "m", "git_branch": None, "is_preview": False, "html_url": None, "git_repository": "ssh://git@gitlab.example:2222/o/r.git"},
    "commit-head": {"commit": "HEAD", "commit_message": "x" * 1200, "git_branch": "main", "is_preview": False, "html_url": None, "git_repository": "not a url"},
}


def cases():
    out = []
    for name, spec in NOTIFICATIONS.items():
        out.append({"name": name, "notification": spec})
    # The team's "commit details" setting.
    out.append({"name": "deploy-success-commit-on", "notification": NOTIFICATIONS["deploy-success-commit"], "commit_details": True})
    out.append({"name": "deploy-failed-commit-on", "notification": NOTIFICATIONS["deploy-failed-commit"], "commit_details": True})
    for name, spec in MAILS.items():
        out.append({"name": name, "mail": spec})
    for name, spec in COMMITS.items():
        out.append({"name": name, "commit": spec})
    return out


def run(*args, **kw):
    p = subprocess.run(args, capture_output=True, text=True, **kw)
    if p.returncode != 0:
        sys.exit(f"{args[:6]} failed: {p.stdout[-1500:]} {p.stderr[-1500:]}")
    return p.stdout


def reference(all_cases):
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "cases.json")
        with open(path, "w") as f:
            json.dump({"cases": all_cases}, f)
        out_path = os.path.join(tmp, "out.json")
        run("docker", "cp", path, f"{CONTAINER}:/tmp/notif-cases.json")
        run("docker", "cp", os.path.join(ROOT, "compat/diff/notifications_ref.php"), f"{CONTAINER}:/tmp/notif-ref.php")
        try:
            run(
                "docker", "exec", CONTAINER, "php", "/var/www/html/artisan", "tinker",
                "--execute", 'require "/tmp/notif-ref.php";',
            )
            run("docker", "cp", f"{CONTAINER}:/tmp/notif-out.json", out_path)
        finally:
            run("docker", "exec", "-u", "root", CONTAINER, "rm", "-f", "/tmp/notif-cases.json", "/tmp/notif-ref.php", "/tmp/notif-out.json")
        with open(out_path) as f:
            return json.load(f)


def rust(ctx, all_cases):
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "cases.json")
        with open(path, "w") as f:
            json.dump({"ctx": ctx, "cases": all_cases}, f)
        out = run("cargo", "run", "-q", "-p", "coolify-jobs", "--example", "notifications_render", "--", path, cwd=ROOT)
    return json.loads(out)


MASK = re.compile(r"<t:\d+:R>|\\u003Ct:\d+:R\\u003E")


def mask(v):
    return MASK.sub("<t:0:R>", v) if isinstance(v, str) else v


def main():
    verbose = "-v" in sys.argv
    all_cases = cases()
    ref = reference(all_cases)
    ctx = ref.pop("__ctx")
    rs = rust(ctx, all_cases)
    compared = same = 0
    failures = []
    for case in all_cases:
        name = case["name"]
        r, o = ref.get(name), rs.get(name)
        if r is None or "__error" in r:
            failures.append((name, "reference", json.dumps(r)[:400], ""))
            continue
        for key in sorted(set(r) | set(o or {})):
            compared += 1
            a, b = mask(r.get(key)), mask((o or {}).get(key))
            if a == b:
                same += 1
            else:
                failures.append((name, key, a, b))
    for name, key, a, b in failures:
        print(f"--- {name} {key}\nref: {a!r}\n rs: {b!r}")
    print(f"{len(all_cases)} cases, {compared} comparisons, {same} same, {compared - same} different")
    if verbose:
        print(json.dumps(ctx))
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
