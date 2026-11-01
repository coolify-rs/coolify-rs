#!/usr/bin/env python3
"""`php artisan schedule:list` of the reference against `coolify_jobs::kernel::schedule()`.

Every line of the reference is in the table with the same cron, or is named here with the reason it
is not. Reads only; nothing runs on either side. Exit status 1 when anything differs.

    python3 compat/diff/kernel_schedule_diff.py
"""
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# Reference line -> why coolify-rs has no entry.
NOT_SCHEDULED = {
    "cleanup:ssh-mux": "no SSH multiplexing: ControlMaster=no",
    "versions:refresh-local": "Coolify Cloud only",
    "php artisan cleanup:redis --clear-locks": "no Redis",
    "php artisan horizon:snapshot": "no Horizon",
    "php artisan uploads:clear": "no chunk directory",
    "php artisan dns:release-orphaned-records": "not ported",
    "php artisan scheduled:dispatch": "Scheduler::tick",
}
# Reference name -> name in the table.
NAMES = {
    "php artisan cleanup:stucked-resources": "cleanup:stucked-resources",
    "php artisan sanctum:prune-expired --hours=1": "sanctum:prune-expired",
    "php artisan cleanup:database-import-uploads": "cleanup:database-import-uploads",
    "php artisan cleanup:unreachable-servers": "cleanup:unreachable-servers",
    "php artisan cleanup:database --yes": "cleanup:database",
}


def reference():
    out = subprocess.run(
        ["docker", "exec", "coolify-rs-reference-coolify-1", "php", "/var/www/html/artisan", "schedule:list"],
        capture_output=True,
        text=True,
    ).stdout
    rows = {}
    for line in out.splitlines():
        m = re.match(r"\s*(\S+\s+\S+\s+\S+\s+\S+\s+\S+)\s+(.+?)\s*\.*\s*Next Due", line)
        if m:
            rows[m.group(2).strip()] = " ".join(m.group(1).split())
    return rows


def coolify_rs():
    out = subprocess.run(
        ["cargo", "test", "-q", "-p", "coolify-jobs", "--lib", "print_schedule", "--", "--ignored", "--nocapture"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    ).stdout
    rows = {}
    for line in out.splitlines():
        if line.startswith("KERNEL "):
            _, name, cron = line.split(" ", 2)
            rows[name] = cron
    return rows


def main():
    ref, rs = reference(), coolify_rs()
    same, different = 0, []
    seen = set()
    for name, cron in ref.items():
        if name in NOT_SCHEDULED:
            print(f"not scheduled: {name} ({NOT_SCHEDULED[name]})")
            continue
        key = NAMES.get(name, name.removeprefix("App\\Jobs\\"))
        seen.add(key)
        if rs.get(key) == cron:
            same += 1
        else:
            different.append(f"{name}: reference {cron!r}, coolify-rs {rs.get(key)!r}")
    # The reference's own timezone and frequency apply to its CDN jobs too.
    extra = sorted(set(rs) - seen)
    # `deleteOldOccurrences` shows as a named closure line in the reference.
    print(f"{same}/{len(ref) - len([n for n in ref if n in NOT_SCHEDULED])} lines have the same cron")
    if extra:
        print("only in coolify-rs:", extra)
    for d in different:
        print("DIFF", d)
    return 1 if different or extra else 0


if __name__ == "__main__":
    sys.exit(main())
