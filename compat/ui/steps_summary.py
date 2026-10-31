#!/usr/bin/env python3
"""Short summary of the evidence files of server_actions.mjs: python3 steps_summary.py <prefix>."""
import glob
import json
import os
import sys

here = os.path.dirname(os.path.abspath(__file__))
for f in sorted(glob.glob(f"{here}/shots/actions/{sys.argv[1]}*.json")):
    d = json.load(open(f))
    print(d["name"])
    if d["errors"]:
        print("  errors", d["errors"])
    print("  toasts", d["toasts"])
    for x in d["stDiff"]:
        print("  state", x[:300])
    print("  only ref", [c[:200] for c in d["onlyRef"]][:12])
    print("  only rs ", [c[:200] for c in d["onlyRs"]][:12])
