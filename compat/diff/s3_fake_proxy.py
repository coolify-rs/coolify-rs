#!/usr/bin/env python3
"""HTTP proxy for a dev instance: `coolify-minio` reaches the local MinIO, every other host is refused.

upstream trusts an S3 row with uuid `minio` and endpoint host `coolify-minio` (`trustedInternalHosts()`), so the
S3 paths run against a local fake without changing `instance_settings`. The reference finds that name on its Docker
network. coolify-rs runs on the Mac, where the name does not resolve, so start it with
HTTP_PROXY=http://127.0.0.1:18990 and this proxy. Nothing can leave the machine through it.
"""
import http.client
import http.server
import sys
import urllib.parse

UPSTREAM = ("127.0.0.1", int(sys.argv[1]) if len(sys.argv) > 1 else 19000)
LISTEN = ("127.0.0.1", int(sys.argv[2]) if len(sys.argv) > 2 else 18990)
HOP = {"proxy-connection", "connection", "keep-alive", "transfer-encoding", "te", "upgrade"}


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def forward(self):
        url = urllib.parse.urlsplit(self.path)
        if url.hostname != "coolify-minio":
            self.send_error(403, "only coolify-minio")
            return
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length) if length else None
        conn = http.client.HTTPConnection(*UPSTREAM, timeout=60)
        path = url.path + (f"?{url.query}" if url.query else "")
        headers = {k: v for k, v in self.headers.items() if k.lower() not in HOP}
        conn.request(self.command, path, body=body, headers=headers)
        resp = conn.getresponse()
        data = resp.read()
        self.send_response(resp.status)
        for k, v in resp.getheaders():
            if k.lower() not in HOP and k.lower() != "content-length":
                self.send_header(k, v)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(data)
        conn.close()

    do_GET = do_PUT = do_POST = do_DELETE = do_HEAD = forward

    def do_CONNECT(self):
        self.send_error(403, "no tunnels")

    def log_message(self, *_):
        pass


http.server.ThreadingHTTPServer(LISTEN, Handler).serve_forever()
