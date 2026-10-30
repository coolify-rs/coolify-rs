#!/usr/bin/env python3
"""Measures how far coolify-rs is from upstream: API operations, web pages, Livewire components.

    scripts/parity.py            summary per area
    scripts/parity.py --missing  also lists everything missing
    scripts/parity.py --json

An API operation counts when `routes/mod.rs` has a RouteSpec for it. A web route counts when the
frontend router has its path. A Livewire component counts when some Svelte or Rust file names its
view or class in a "port of" comment, so this is a coverage guide, not a behaviour proof; the
differential scripts in compat/diff and the screenshots are the proof.
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
            if m == "HEAD":
                continue
            out.append((m, "/" + r["uri"], (m, r["uri"]) in ported))
    return out


def web():
    src = read(f"{U}/routes/web.php")
    routes = sorted(set(re.findall(r"Route::get\('([^']+)'", src)))
    front = read(f"{ROOT}/frontend/src/lib/router.svelte.ts")
    have = set()
    for p in re.findall(r"pattern: '([^']+)'", front):
        have.add(re.sub(r":\w+", "{}", p))
    out = []
    for r in routes:
        norm = "/" + re.sub(r"\{[^}]+\}", "{}", r).strip("/")
        out.append((norm, norm in have or (norm == "/" and "/" in have)))
    return out


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
        out.append((rel, any(n in text for n in names)))
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
