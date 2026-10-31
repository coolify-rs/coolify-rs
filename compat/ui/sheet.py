#!/usr/bin/env python3
"""sheet.py out name [name...]: stacks the reference/coolify-rs pairs of several shots into shots/<out>.png."""
import os
import sys
from PIL import Image

base = os.path.join(os.path.dirname(os.path.abspath(__file__)), "shots")
rows = []
for name in sys.argv[2:]:
    a = Image.open(f"{base}/{name}-ref.png")
    b = Image.open(f"{base}/{name}-rs.png")
    w, h = a.width // 2, max(a.height, b.height) // 2
    row = Image.new("RGB", (w * 2 + 8, h), (255, 0, 128))
    row.paste(a.resize((w, a.height // 2)), (0, 0))
    row.paste(b.resize((w, b.height // 2)), (w + 8, 0))
    rows.append(row)
out = Image.new("RGB", (max(r.width for r in rows), sum(r.height for r in rows)), (255, 0, 128))
y = 0
for r in rows:
    out.paste(r, (0, y))
    y += r.height
out.save(f"{base}/{sys.argv[1]}.png")
print(f"{base}/{sys.argv[1]}.png")
