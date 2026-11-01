#!/usr/bin/env python3
"""Triage of a full_pass.mjs run: worst first, changed pixels, final url, title, text and class outline differences.

    full_triage.py PREFIX [--text] [--name=substring]
"""
import difflib
import glob
import json
import os
import re
import sys

from PIL import Image, ImageChops

HERE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "shots")
prefix = sys.argv[1]
show = "--text" in sys.argv
only = next((a.split("=")[1] for a in sys.argv if a.startswith("--name=")), "")


def pixels(name):
    try:
        a = Image.open(f"{HERE}/{prefix}-{name}-ref.png").convert("L")
        b = Image.open(f"{HERE}/{prefix}-{name}-rs.png").convert("L")
    except OSError:
        return 100.0, None, None
    size = (max(a.width, b.width), max(a.height, b.height))
    pa, pb = Image.new("L", size), Image.new("L", size)
    pa.paste(a)
    pb.paste(b)
    d = ImageChops.difference(pa, pb).point(lambda v: 255 if v > 24 else 0)
    d.paste(0, (0, 0, 224, 50))
    share = sum(1 for v in d.getdata() if v) / (size[0] * size[1])
    return share * 100, d.getbbox(), (a.size, b.size)


def lines(s):
    return [re.sub(r"\s+", " ", x).strip() for x in (s or "").split("\n") if x.strip()]


# The product name in the sidebar, and the real-time banner, which stays out of coolify-rs on purpose.
EXPECTED = {"Coolify", "coolify-rs", "Cannot connect to real-time service", "This will cause unusual problems on the UI. Open the required ports or get help on Discord.", "View Docs", "Acknowledge & Disable"}
rows = []
for path in sorted(glob.glob(f"{HERE}/{prefix}-*.json")):
    name = os.path.basename(path)[len(prefix) + 1:-5]
    if only and only not in name:
        continue
    d = json.load(open(path))
    r, s = d["ref"], d["rs"]
    # The product name is compared as one, and a bare number is the counter of deployments running elsewhere.
    rname = [x.replace("Coolify", "coolify-rs") for x in lines(r.get("text"))]
    sname = [x.replace("Coolify", "coolify-rs") for x in lines(s.get("text"))]
    tl = [x for x in difflib.ndiff(rname, sname) if x[0] in "+-" and x[2:] not in EXPECTED and not x[2:].isdigit()]
    ol = [x for x in difflib.unified_diff(r.get("outline", []), s.get("outline", []), lineterm="", n=0) if x[0] in "+-" and not x.startswith(("+++", "---"))]
    share, box, sizes = pixels(name)
    flags = []
    if r.get("url") != s.get("url"):
        flags.append(f"URL {r.get('url')} != {s.get('url')}")
    if r.get("status") != s.get("status"):
        flags.append(f"STATUS {r.get('status')} != {s.get('status')}")
    if r.get("error") or s.get("error"):
        flags.append(f"ERR ref={r.get('error')} rs={s.get('error')}")
    rows.append((share, name, box, sizes, tl, ol, flags, s.get("title"), r.get("title")))

rows.sort(key=lambda x: -x[0])
for share, name, box, sizes, tl, ol, flags, st, rt in rows:
    size = "" if not sizes or sizes[0] == sizes[1] else f" size {sizes[0][1]}/{sizes[1][1]}"
    print(f"{share:6.2f}% t{len(tl):<3} o{len(ol):<4} {name}{size} {' '.join(flags)}")
    if show:
        for x in tl[:24]:
            print("        ", x[:150])
print(f"total {len(rows)}; text-identical {sum(1 for r in rows if not r[4])}; outline-identical {sum(1 for r in rows if not r[5])}")
