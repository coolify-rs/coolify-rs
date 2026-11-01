"""A stand-in `coolify-sentinel` for compat/diff/metrics_diff.py, in the shape of Sentinel 1.1.3's real
answers. Live Sentinel values differ between two reads, so a comparison of them means little. These are a
function of the timestamp alone and stop at the last full minute, so both sides read the same thing.

The token selects a profile: busy, calm, or old (a Sentinel without summary, disk, load and network).
"""

import json
import math
import os
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse
import datetime

PROFILES = json.loads(os.environ["PROFILES"])  # {token: "busy" | "calm" | "old"}
CONTAINERS = json.loads(os.environ["CONTAINERS"])  # ids, in the order Sentinel lists them
MB = 1024 * 1024
GB = 1024 * MB
TOTAL = 16 * GB


def wave(t, base, spread, period=900.0):
    return base + spread * math.sin(t / 1000.0 / period) + (t // 5000 % 7) / 10.0


def times(from_ms):
    now = int(time.time()) // 60 * 60 * 1000
    day_ago = now - 86_400_000
    hour_ago = now - 3_600_000
    t = max(from_ms, now - 31 * 86_400_000)
    # Sparser than a real Sentinel's samples: scripts/ssh-via-orb.sh can cut an answer at 2 MiB.
    out = []
    if t < day_ago:
        start = t - t % 300_000
        out += range(start, day_ago, 300_000)
    if t < hour_ago:
        start = max(t, day_ago)
        out += range(start - start % 30_000, hour_ago, 30_000)
    start = max(t, hour_ago)
    out += range(start - start % 5000, now + 1, 5000)
    return [x for x in out if x >= from_ms]


def cpu(t, base):
    return {"time": str(t), "percent": f"{wave(t, base, 8):.2f}"}


def memory(t, used):
    value = int(used + wave(t, 0, 200 * MB))
    return {"time": str(t), "total": TOTAL, "available": TOTAL - value, "used": value, "usedPercent": round(value / TOTAL * 100, 2), "free": TOTAL - value}


def disks(t, percent):
    used = int(100 * GB * percent / 100)
    return [
        {"time": str(t), "mount": "/", "total": 100 * GB, "used": used, "available": 100 * GB - used, "usedPercent": percent},
        {"time": str(t), "mount": "/data", "total": 500 * GB, "used": 400 * GB, "available": 100 * GB, "usedPercent": 80.0},
    ]


def container_row(index, cid, profile):
    now = str(int(time.time()) // 60 * 60 * 1000)
    # A removed container keeps its last samples, without a status.
    removed = cid.startswith("removed")
    row = {
        "id": cid,
        "cpu": {"time": now, "percent": f"{(len(CONTAINERS) - index) * (3.5 if profile == 'busy' else 1.25):.2f}"},
        "memory": {"time": now, "total": TOTAL, "available": TOTAL, "used": (index + 1) * 37 * MB, "usedPercent": round((index + 1) * 37 * MB / TOTAL * 100, 2), "free": TOTAL},
        "disk": {"time": now, "writableLayer": (index + 2) * 11 * MB, "volumesTotal": 5 * GB if index == 1 else 0},
        "network": {"time": now, "rxBytesPerSec": index * 1500.75, "txBytesPerSec": (len(CONTAINERS) - index) * 700.5},
        "status": None if removed else {"state": "running", "health": "healthy", "restartCount": 0},
        "time": now,
    }
    return row


def answer(path, query, profile):
    base = {"busy": 78.0, "calm": 11.0, "old": 25.0}[profile]
    from_text = query.get("from", ["1970-01-01T00:00:00Z"])[0]
    from_ms = int(datetime.datetime.fromisoformat(from_text.replace("Z", "+00:00")).timestamp() * 1000)
    parts = path.strip("/").split("/")
    if parts[:2] == ["api", "container"] and len(parts) == 5:
        cid = parts[2]
        seed = sum(ord(c) for c in cid) % 20 + 2
        if parts[3] == "cpu":
            return [cpu(t, seed) for t in times(from_ms)]
        if parts[3] == "memory":
            return [memory(t, seed * 40 * MB) for t in times(from_ms)]
        return None
    route = "/".join(parts[1:])
    if route == "cpu/history":
        return [cpu(t, base) for t in times(from_ms)]
    if route == "memory/history":
        return [memory(t, TOTAL * (0.75 if profile == "busy" else 0.4)) for t in times(from_ms)]
    if profile == "old":
        return None
    if route == "summary":
        now = str(int(time.time()) // 60 * 60 * 1000)
        return {
            "cpu": {"time": now, "percent": base},
            "memory": memory(0, TOTAL * (0.75 if profile == "busy" else 0.4)),
            "disk": disks(now, 85.5 if profile == "busy" else 21.75),
            "network": {"time": now, "rxBytesPerSec": 123456.75, "txBytesPerSec": 7890.25},
            "load": {"time": now, "load1": 7.93 if profile == "busy" else 0.42, "load5": 6.69, "load15": 3.59},
            "host": {"time": now, "uptimeSeconds": 115191, "swapTotal": 0, "swapUsed": 0, "swapFree": 0, "swapUsedPercent": 0.0},
        }
    if route == "containers/current":
        return [container_row(i, cid, profile) for i, cid in enumerate(CONTAINERS)]
    if route == "disk/history":
        return [row for t in times(from_ms) for row in disks(t, round(wave(t, 40, 5), 2))]
    if route == "load/history":
        return [{"time": str(t), "load1": round(wave(t, 2, 1), 2), "load5": 1.0, "load15": 0.5} for t in times(from_ms)]
    if route == "network/history":
        return [{"time": str(t), "rxBytesPerSec": round(wave(t, 5000, 2000), 2), "txBytesPerSec": round(wave(t, 900, 300), 2)} for t in times(from_ms)]
    return None


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def send(self, status, text):
        body = text.encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        url = urlparse(self.path)
        if url.path == "/api/version":
            return self.send(200, "1.1.3")
        if url.path == "/api/health":
            return self.send(200, "{}")
        token = self.headers.get("Authorization", "").removeprefix("Bearer ")
        profile = PROFILES.get(token)
        if profile is None:
            return self.send(401, json.dumps({"error": "Unauthorized"}))
        result = answer(url.path, parse_qs(url.query), profile)
        if result is None:
            return self.send(404, "404 page not found")
        self.send(200, json.dumps(result, separators=(",", ":")))


ThreadingHTTPServer(("127.0.0.1", 8888), Handler).serve_forever()
