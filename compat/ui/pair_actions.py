#!/usr/bin/env python3
"""Pairs evidence screenshots of server_actions.mjs: python3 pair_actions.py <prefix> <name>..., writes shots/<prefix>_<name>-pair.png."""
import os
import shutil
import subprocess
import sys

here = os.path.dirname(os.path.abspath(__file__))
prefix = sys.argv[1]
for name in sys.argv[2:]:
    short = f"{prefix}_{name}"
    for side in ("ref", "rs"):
        shutil.copy(f"{here}/shots/actions/{short}-{side}.png", f"{here}/shots/{short}-{side}.png")
    subprocess.run(["python3", f"{here}/pair.py", short], check=True, cwd=here, stdout=subprocess.DEVNULL)
    print(f"{here}/shots/{short}-pair.png")
