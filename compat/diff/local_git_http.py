#!/usr/bin/env python3
"""Smart HTTP for one bare repository, so deployments clone locally instead of from GitHub.

    local_git_http.py <bare repository> <port> <request log>

Runs on the test VM. Every path `/<owner>/<name>[.git]/...` serves the repository. Any commit can be
fetched by its hash, as on GitHub, because deployments of a rollback ask for one.
"""
import gzip
import http.server
import os
import subprocess
import sys

REPO, PORT, LOG = sys.argv[1], int(sys.argv[2]), sys.argv[3]
ROOT, NAME = os.path.split(REPO.rstrip("/"))
CONFIG = {"safe.directory": "*", "uploadpack.allowAnySHA1InWant": "true", "uploadpack.allowFilter": "true", "http.uploadpack": "true"}


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def serve(self):
        path, _, query = self.path.partition("?")
        with open(LOG, "a") as f:
            f.write(f"{self.command} {self.path}\n")
        parts = path.split("/", 3)
        if len(parts) < 4:
            self.send_error(404)
            return
        env = dict(os.environ, GIT_PROJECT_ROOT=ROOT, GIT_HTTP_EXPORT_ALL="1", PATH_INFO=f"/{NAME}/{parts[3]}", QUERY_STRING=query,
                   REQUEST_METHOD=self.command, CONTENT_TYPE=self.headers.get("Content-Type", ""), REMOTE_ADDR=self.client_address[0],
                   GIT_PROTOCOL=self.headers.get("Git-Protocol", ""), GIT_CONFIG_COUNT=str(len(CONFIG)))
        for i, (k, v) in enumerate(CONFIG.items()):
            env[f"GIT_CONFIG_KEY_{i}"], env[f"GIT_CONFIG_VALUE_{i}"] = k, v
        body = self.rfile.read(int(self.headers.get("Content-Length") or 0))
        if self.headers.get("Content-Encoding") == "gzip":
            body = gzip.decompress(body)
        out = subprocess.run(["git", "http-backend"], input=body, env=env, capture_output=True).stdout
        head, _, rest = out.partition(b"\r\n\r\n")
        status, headers = 200, []
        for line in head.decode().split("\r\n"):
            k, _, v = line.partition(": ")
            if k.lower() == "status":
                status = int(v.split()[0])
            elif k:
                headers.append((k, v))
        self.send_response(status)
        for k, v in headers:
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(rest)))
        self.end_headers()
        self.wfile.write(rest)

    do_GET = do_POST = serve


http.server.ThreadingHTTPServer(("0.0.0.0", PORT), Handler).serve_forever()
