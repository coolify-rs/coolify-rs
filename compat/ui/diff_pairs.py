#!/usr/bin/env python3
"""How much two screenshots differ: share of changed pixels, without the logo, the version and the popup corners.

    diff_pairs.py name [name ...]     reads shots/<name>-{ref,rs}.png, prints worst first
"""
import os
import sys

from PIL import Image, ImageChops

HERE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "shots")
rows = []
for name in sys.argv[1:]:
    a = Image.open(os.path.join(HERE, f"{name}-ref.png")).convert("L")
    b = Image.open(os.path.join(HERE, f"{name}-rs.png")).convert("L")
    if a.size != b.size:
        rows.append((1.0, name))
        continue
    d = ImageChops.difference(a, b).point(lambda v: 255 if v > 24 else 0)
    # Logo and version; the deployments counter depends on what else runs.
    d.paste(0, (0, 0, 224, 50))
    d.paste(0, (1200, 0, 1290, 50))
    box = d.getbbox()
    share = sum(1 for v in d.getdata() if v) / (a.size[0] * a.size[1])
    rows.append((share, f"{name} bbox={box}"))
for share, name in sorted(rows, key=lambda r: -r[0]):
    print(f"{share*100:6.2f}% {name}")
