"""Acts as Discord, Slack, Telegram, Pushover, Resend and webhook endpoints for
compat/diff/notifications_e2e.py. A path with /status/NNN/ answers NNN."""

import base64
import json
import re
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOG = []
LOCK = threading.Lock()


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def answer(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        with LOCK:
            self.answer(200, LOG)

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length)
        if self.path == "/__reset":
            with LOCK:
                LOG.clear()
            self.answer(200, {"ok": True})
            return
        entry = {
            "path": self.path,
            "headers": {k.lower(): v for k, v in self.headers.items()},
            "body": base64.b64encode(body).decode(),
        }
        with LOCK:
            LOG.append(entry)
        m = re.search(r"/status/(\d+)", self.path)
        if m:
            self.answer(int(m.group(1)), {"message": "from listener"})
            return
        if "/resend" in self.path:
            self.answer(200, {"id": "resend-id"})
            return
        self.answer(200, {"ok": True})


ThreadingHTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
