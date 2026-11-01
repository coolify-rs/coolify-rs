#!/usr/bin/env python3
"""Measures how far coolify-rs is from upstream: API operations, web pages, Livewire components.

    scripts/parity.py            summary per area
    scripts/parity.py --missing  also lists everything missing
    scripts/parity.py --json

An API operation counts when `routes/mod.rs` has a RouteSpec for it. A web route, with its
`Route::prefix` groups applied, counts when the frontend router has its path or the backend serves it. A Livewire component counts when some Svelte or Rust file names its
view or class in a "port of" comment, so this is a coverage guide, not a behaviour proof; the
differential scripts in compat/diff and the screenshots are the proof. `INLINE`, `DEV_ONLY_*` and
`API_CATCH_ALL` hold what was opened by hand and is not a gap.
"""
import collections
import glob
import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
U = os.path.join(ROOT, ".upstream/snapshot")


def read(p):
    return open(p, encoding="utf-8", errors="replace").read()


def api():
    inv = json.load(open(f"{ROOT}/compat/api/upstream-routes.json"))
    rows = inv["routes"] if isinstance(inv, dict) else inv
    ported = set()
    src = read(f"{ROOT}/crates/coolify-api/src/routes/mod.rs")
    for m, u in re.findall(r'method:\s*"([A-Z]+)",\s*uri:\s*"([^"]+)"', src):
        ported.add((m, u))
    out = []
    for r in rows:
        for m in r["methods"]:
            if m == "HEAD" or "/" + r["uri"] in API_CATCH_ALL:
                continue
            out.append((m, "/" + r["uri"], (m, r["uri"]) in ported))
    return out


def web_routes():
    """Every `Route::get` in routes/web.php with its enclosing `Route::prefix` groups applied."""
    routes = set()
    stack = []  # (brace depth inside the group, prefix)
    depth = 0
    pending = None
    for line in read(f"{U}/routes/web.php").splitlines():
        if line.lstrip().startswith("//"):
            continue
        m = re.search(r"Route::prefix\('([^']*)'\)", line)
        if m:
            pending = m.group(1)
        for g in re.findall(r"Route::get\('([^']+)'", line):
            parts = [x.strip("/") for _, x in stack] + [g.strip("/")]
            routes.add("/".join(x for x in parts if x))
        code = re.sub(r"'[^']*'", "''", line)
        opens_group = "group(function" in code
        for ch in code:
            if ch == "{":
                depth += 1
                if opens_group:
                    stack.append((depth, pending or ""))
                    pending, opens_group = None, False
            elif ch == "}":
                while stack and stack[-1][0] == depth:
                    stack.pop()
                depth -= 1
    return sorted(routes)


# Router params that take any literal: the page behind them decides what renders.
CATCH_ALL = {":section", ":sub", ":tab", ":backupSection", ":channel"}


def web():
    """A web route counts when the frontend router has a pattern for it. A literal segment that only
    lands in a catch-all section param counts only when the frontend also names that literal."""
    front = read(f"{ROOT}/frontend/src/lib/router.svelte.ts")
    patterns = [[s for s in p.split("/") if s] for p in re.findall(r"pattern: '([^']+)'", front)]
    corpus = "\n".join(
        read(f)
        for ext in ("svelte", "ts")
        for f in glob.glob(f"{ROOT}/frontend/src/**/*.{ext}", recursive=True)
    )

    def named(lit):
        return re.search(r"""['"`/]""" + re.escape(lit) + r"""['"`]""", corpus) is not None

    def covered(segs):
        for pat in patterns:
            if len(pat) != len(segs):
                continue
            ok = True
            for want, got in zip(segs, pat):
                if want == "{}":
                    ok = got.startswith(":")
                elif got.startswith(":"):
                    ok = got in CATCH_ALL and named(want)
                else:
                    ok = want == got
                if not ok:
                    break
            if ok:
                return True
        return False

    # Routes the backend answers itself: downloads, avatars, icons and /canvas/api.
    served = set()
    for f in glob.glob(f"{ROOT}/crates/coolify-api/src/**/*.rs", recursive=True):
        text = read(f)
        for path in re.findall(r"""\.route\(\s*"(/[^"]*)\"""", text):
            served.add(re.sub(r"\{[^}]+\}", "{}", path).rstrip("/"))
        if f.endswith("ui_canvas.rs"):
            for path in re.findall(r"""\(\s*"(?:GET|POST|PATCH)",\s*"([^"]+)",?\s*\)""", text):
                served.add("/canvas/api/" + re.sub(r"\{[^}]+\}", "{}", path))

    out = []
    for r in web_routes():
        norm = "/" + re.sub(r"\{[^}]+\}", "{}", r).strip("/")
        if norm in DEV_ONLY_PAGES:
            continue
        out.append((norm, norm in served or covered([s for s in norm.split("/") if s])))
    return out


# Components ported inside another file under no name of their own. Each entry names the file and text
# that hold the port, and only counts while that text is still there.
INLINE = {
    "Concerns/BuildsMetricsChartPayload": ("crates/coolify-api/src/ui_fleet_metrics.rs", "fn build_chart_payload"),
    "Concerns/BuildsTrafficChartPayload": ("crates/coolify-api/src/ui_server_analytics.rs", "`BuildsTrafficChartPayload`"),
    "Project/Application/Preview/Form": ("crates/coolify-api/src/ui_shared/previews.rs", "`Preview\\Form::submit`"),
    "Project/Application/ServerStatusBadge": ("frontend/src/pages/Application.svelte", "Application is in degraded state across multiple servers."),
    "Project/Application/Status": ("frontend/src/components/ApplicationHeading.svelte", "DatabaseStatusSummary {status} {healthcheckUrl}"),
    "Project/New/GiteaPrivateRepository": ("crates/coolify-api/src/ui_applications/private_git.rs", "GiteaPrivateRepository"),
    "Project/New/GitlabPrivateRepository": ("crates/coolify-api/src/ui_applications/private_git.rs", "GitlabPrivateRepository"),
    "Project/Service/Analytics": ("frontend/src/pages/Service.svelte", '<ResourceAnalytics kind="service"'),
    "Project/Shared/ScheduledTask/Add": ("frontend/src/components/shared/ScheduledTasks.svelte", "async function submitAdd"),
}

# Only registered under `app()->environment(['local', 'testing'])`: a production upstream has neither.
DEV_ONLY_PAGES = {"/__livewire-request-failure"}
DEV_ONLY_LIVEWIRE = {"Dev/LivewireRequestFailurePreview"}
# `Route::any('/{any}')` in routes/api.php: not an operation, the answer for every unknown path.
API_CATCH_ALL = {"/api/{any}"}


def inline(rel):
    if rel not in INLINE:
        return False
    path, marker = INLINE[rel]
    return marker in read(f"{ROOT}/{path}")


def livewire():
    corpus = []
    for pat in ("frontend/src/**/*.svelte", "frontend/src/**/*.ts", "crates/**/*.rs"):
        for f in glob.glob(f"{ROOT}/{pat}", recursive=True):
            corpus.append(read(f))
    text = "\n".join(corpus).lower()
    out = []
    base = f"{U}/app/Livewire/"
    for f in sorted(glob.glob(base + "**/*.php", recursive=True)):
        rel = f[len(base):-4]
        parts = rel.split("/")
        kebab = "/".join(re.sub(r"(?<!^)(?=[A-Z])", "-", p).lower() for p in parts)
        names = {kebab + ".blade.php", rel.replace("/", "\\").lower(), rel.lower()}
        if rel in DEV_ONLY_LIVEWIRE:
            continue
        out.append((rel, inline(rel) or any(n in text for n in names)))
    return out


def main():
    a, w, l = api(), web(), livewire()
    area = lambda u: (re.sub(r"^/api/v1/?", "", u).split("/")[0] or "root")
    if "--json" in sys.argv:
        print(json.dumps({"api": a, "web": w, "livewire": l}))
        return
    def line(label, items):
        done = sum(1 for i in items if i[-1])
        print(f"{label:<14} {done:>4}/{len(items):<4} {100 * done // max(len(items), 1):>3}%")
    line("API ops", a)
    line("Web pages", w)
    line("Livewire", l)
    print("\nAPI by area:")
    groups = collections.defaultdict(list)
    for x in a:
        groups[area(x[1])].append(x)
    for k in sorted(groups, key=lambda k: -len(groups[k])):
        line(k, groups[k])
    print("\nLivewire by area:")
    groups = collections.defaultdict(list)
    for x in l:
        groups["/".join(x[0].split("/")[:2]) if x[0].startswith("Project") else x[0].split("/")[0]].append(x)
    for k in sorted(groups, key=lambda k: -len(groups[k])):
        line(k, groups[k])
    if "--missing" in sys.argv:
        print("\nMissing API:")
        for m, u, ok in a:
            if not ok:
                print(f"  {m} {u}")
        print("\nMissing pages:")
        for u, ok in w:
            if not ok:
                print(f"  {u}")
        print("\nMissing Livewire:")
        for r, ok in l:
            if not ok:
                print(f"  {r}")


main()
