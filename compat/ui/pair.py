#!/usr/bin/env python3
"""pair.py name [name...]: compat/ui/shots/<name>-pair.png, reference left, coolify-rs right."""
import os
import sys
from PIL import Image
base = os.path.join(os.path.dirname(os.path.abspath(__file__)), "shots")
for name in sys.argv[1:]:
    a = Image.open(f"{base}/{name}-ref.png")
    b = Image.open(f"{base}/{name}-rs.png")
    w, h = a.width // 2, max(a.height, b.height) // 2
    out = Image.new("RGB", (w * 2 + 8, h), (255, 0, 128))
    out.paste(a.resize((w, a.height // 2)), (0, 0))
    out.paste(b.resize((w, b.height // 2)), (w + 8, 0))
    out.save(f"{base}/{name}-pair.png")
    print(f"{base}/{name}-pair.png")
