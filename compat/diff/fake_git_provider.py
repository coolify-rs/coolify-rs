#!/usr/bin/env python3
"""A local stand-in for the GitHub, GitLab and Gitea APIs, used by the git source differential
scripts. It is no proof that a real provider behaves like this: it only answers what the code
paths under test ask for, in the documented shapes.

    fake_git_provider.py [--port 18990] [--repo-dir DIR]

Serves on 0.0.0.0, so a reference container, coolify-rs and a test VM can all reach it:

    GitHub  {base}/github/...   html_url {base}/gh, api_url {base}/github
    GitLab  {base}/gitlab/...   html_url {base}/gl, api_url {base}/gitlab/api/v4
    Gitea   {base}/gitea/...    html_url {base}/gt, api_url {base}/gitea/api/v1
    Git     {base}/{gh,gl,gt}/o/r.git over smart HTTP (`git http-backend`), any credentials

The bare repository of `o/r` is `--repo-dir/r.git`, created on first use with a small Dockerfile
app when it does not exist. `GET /__log` returns every API request so far, `POST /__reset` empties
it, `POST /__state` merges JSON into the behaviour switches.
"""
import argparse
import base64
import json
import os
import subprocess
import threading
import time
from email.utils import formatdate
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit

STATE = {"token_expires_in": 7200, "github_app_id": 4242, "fail_tokens": False, "clock_skew": 0}
LOG = []
LOCK = threading.Lock()
REPO_DIR = "/tmp/fake-git"
PEM = None
HEAD = "0" * 40


def repo_id(name):
    return 1000 + sum(ord(c) for c in name)


def github_repo(name="r", owner="o"):
    return {
        "id": repo_id(name),
        "name": name,
        "full_name": f"{owner}/{name}",
        "private": True,
        "owner": {"login": owner},
        "default_branch": "main",
        "html_url": f"http://fake/gh/{owner}/{name}",
    }


def ensure_repo():
    path = os.path.join(REPO_DIR, "r.git")
    if os.path.isdir(path):
        return path
    os.makedirs(REPO_DIR, exist_ok=True)
    work = os.path.join(REPO_DIR, "work")
    os.makedirs(work, exist_ok=True)
    with open(os.path.join(work, "Dockerfile"), "w") as f:
        f.write("FROM nginx:alpine\nEXPOSE 80\n")
    with open(os.path.join(work, "docker-compose.yaml"), "w") as f:
        f.write("services:\n  web:\n    image: nginx:alpine\n    ports:\n      - '80'\n")
    env = dict(os.environ, GIT_AUTHOR_NAME="fake", GIT_AUTHOR_EMAIL="fake@example.test",
               GIT_COMMITTER_NAME="fake", GIT_COMMITTER_EMAIL="fake@example.test")
    run = lambda *a, cwd=work: subprocess.run(a, cwd=cwd, env=env, check=True, capture_output=True)
    run("git", "init", "-q", "-b", "main")
    run("git", "add", ".")
    run("git", "commit", "-qm", "Initial")
    run("git", "branch", "feature")
    run("git", "update-ref", "refs/pull/7/head", "HEAD")
    run("git", "update-ref", "refs/merge-requests/7/head", "HEAD")
    subprocess.run(["git", "clone", "-q", "--bare", "--mirror", work, path], env=env, check=True)
    subprocess.run(["git", "config", "http.receivepack", "false"], cwd=path, check=True)
    subprocess.run(["git", "update-server-info"], cwd=path, check=True)
    return path


def head_sha():
    path = ensure_repo()
    return subprocess.run(["git", "rev-parse", "main"], cwd=path, capture_output=True, text=True).stdout.strip()


def pem():
    global PEM
    if PEM is None:
        PEM = subprocess.run(["openssl", "genrsa", "2048"], capture_output=True, text=True, check=True).stdout
    return PEM


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def date_time_string(self, timestamp=None):
        return formatdate(time.time() + STATE["clock_skew"], usegmt=True)

    def body(self):
        n = int(self.headers.get("Content-Length") or 0)
        return self.rfile.read(n) if n else b""

    def send(self, status, data, headers=None):
        raw = data if isinstance(data, bytes) else json.dumps(data).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.send_header("X-RateLimit-Remaining", "4999")
        self.send_header("X-RateLimit-Reset", "1893456000")
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(raw)

    def git(self, path, query):
        """Smart HTTP through `git http-backend`."""
        ensure_repo()
        env = dict(os.environ, GIT_PROJECT_ROOT=REPO_DIR, GIT_HTTP_EXPORT_ALL="1", PATH_INFO=path,
                   QUERY_STRING=query, REQUEST_METHOD=self.command,
                   CONTENT_TYPE=self.headers.get("Content-Type", ""), REMOTE_USER="fake",
                   REMOTE_ADDR=self.client_address[0])
        body = self.body()
        if self.headers.get("Content-Encoding") == "gzip":
            import gzip
            body = gzip.decompress(body)
        out = subprocess.run(["git", "http-backend"], input=body, env=env, capture_output=True).stdout
        head, _, rest = out.partition(b"\r\n\r\n")
        status = 200
        headers = {}
        for line in head.decode().split("\r\n"):
            k, _, v = line.partition(": ")
            if k.lower() == "status":
                status = int(v.split()[0])
            elif k:
                headers[k] = v
        self.send_response(status)
        for k, v in headers.items():
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(rest)))
        self.end_headers()
        self.wfile.write(rest)

    def handle_any(self):
        parts = urlsplit(self.path)
        path, query = parts.path, parts.query
        for prefix in ("/gh/", "/gl/", "/gt/"):
            if path.startswith(prefix):
                # /gh/o/r.git/info/refs -> /r.git/info/refs
                segs = path[len(prefix):].split("/", 1)
                if len(segs) == 2:
                    return self.git("/" + segs[1], query)
        # GitHub App clones drop the path of html_url (and its port).
        if path.startswith("/o/"):
            return self.git("/" + path[len("/o/"):], query)
        if path == "/__log":
            with LOCK:
                return self.send(200, LOG)
        if path == "/__reset":
            with LOCK:
                LOG.clear()
            self.body()
            return self.send(200, {"ok": True})
        if path == "/__state":
            STATE.update(json.loads(self.body() or b"{}"))
            return self.send(200, STATE)
        raw = self.body()
        q = {k: v[0] for k, v in parse_qs(query).items()}
        try:
            data = json.loads(raw) if raw and "json" in (self.headers.get("Content-Type") or "") else None
        except ValueError:
            data = None
        form = {k: v[0] for k, v in parse_qs(raw.decode(errors="replace")).items()} if data is None and raw else {}
        with LOCK:
            LOG.append({
                "method": self.command, "path": path, "query": q, "json": data, "form": sorted(form),
                "auth": (self.headers.get("Authorization") or "").split(" ")[0],
                "accept": self.headers.get("Accept"),
            })
        if path.startswith("/github"):
            return self.github(path[len("/github"):], q, data)
        if path.startswith("/gitlab"):
            return self.gitlab(path[len("/gitlab"):], q, data, form)
        if path.startswith("/gitea"):
            return self.gitea(path[len("/gitea"):], q, data, form)
        return self.send(404, {"message": "Not Found"})

    do_GET = do_POST = do_PATCH = do_PUT = do_DELETE = handle_any

    def github(self, p, q, data):
        m = self.command
        if p == "/zen":
            return self.send(200, b"Keep it logically awesome.")
        if p == "/app":
            return self.send(200, {
                "id": STATE["github_app_id"], "slug": "fake-app", "name": "Fake App",
                "permissions": {"contents": "read", "metadata": "read", "pull_requests": "write", "administration": "read"},
                "events": ["push", "pull_request"],
            })
        if p.startswith("/app/installations/"):
            rest = p[len("/app/installations/"):]
            if rest.endswith("/access_tokens"):
                if STATE["fail_tokens"]:
                    return self.send(404, {"message": "Not Found"})
                return self.send(201, {"token": "ghs_fake+token/1", "expires_at": "2099-01-01T00:00:00Z"})
            return self.send(200, {"id": int(rest), "app_id": STATE["github_app_id"]})
        if p == "/installation/repositories":
            page = int(q.get("page", 1))
            repos = [github_repo("zeta"), github_repo("r"), github_repo("alpha")] if page == 1 else []
            return self.send(200, {"total_count": 3, "repositories": repos})
        if p.startswith("/app-manifests/") and p.endswith("/conversions"):
            code = p.split("/")[2]
            if code == "bad":
                return self.send(404, {"message": "Not Found"})
            return self.send(201, {
                "id": STATE["github_app_id"], "slug": "fake-app", "client_id": "Iv1.fake",
                "client_secret": "fake-client-secret", "pem": pem(), "webhook_secret": "fake-webhook-secret",
            })
        if p.startswith("/repos/o/"):
            segs = p.split("/")[3:]
            name = segs[0]
            if name == "missing":
                return self.send(404, {"message": "Not Found"})
            if len(segs) == 1:
                return self.send(200, github_repo(name))
            if segs[1] == "branches":
                if len(segs) > 2:
                    return self.send(200, {"name": segs[2], "commit": {"sha": head_sha()}})
                return self.send(200, [{"name": "main", "commit": {"sha": head_sha()}}, {"name": "feature", "commit": {"sha": head_sha()}}])
            if segs[1] == "compare":
                return self.send(200, {"files": [{"filename": "src/a.js"}, {"filename": "docs/new.md", "previous_filename": "docs/old.md"}]})
            if segs[1] == "commits":
                return self.send(200, {"commit": {"message": "Fake commit message"}})
            if segs[1] == "pulls" and len(segs) == 4 and segs[3] == "files":
                return self.send(200, [{"filename": "src/a.js"}])
            if segs[1] == "issues" and segs[-1] == "comments":
                return self.send(201, {"id": 555})
            if segs[1] == "issues" and "comments" in segs:
                return self.send(200 if m == "PATCH" else 204, {"id": int(segs[-1])})
        return self.send(404, {"message": "Not Found"})

    def tokens(self):
        return self.send(200, {
            "access_token": "oauth+access/" + str(int(time.time())), "refresh_token": "oauth-refresh",
            "expires_in": STATE["token_expires_in"], "token_type": "bearer",
        })

    def gitlab(self, p, q, data, form):
        if p == "/oauth/token":
            return self.tokens()
        if p == "/api/v4/projects":
            if int(q.get("page", 1)) > 1:
                return self.send(200, [])
            return self.send(200, [{
                "id": 77, "name": "r", "path_with_namespace": "o/r", "default_branch": "main",
                "web_url": "http://fake/gl/o/r", "namespace": {"full_path": "o", "kind": "group"},
            }])
        if p == "/api/v4/projects/77/repository/branches":
            return self.send(200, [{"name": "main"}, {"name": "feature"}])
        if p.startswith("/api/v4/projects/") and "/notes" in p:
            return self.send(201 if self.command == "POST" else 200, {"id": 556})
        if p.startswith("/api/v4/projects/"):
            return self.send(200, {"id": 77, "path_with_namespace": "o/r", "default_branch": "main"})
        return self.send(404, {"message": "404 Not Found"})

    def gitea(self, p, q, data, form):
        if p == "/login/oauth/access_token":
            return self.tokens()
        if p == "/api/v1/user/repos":
            if int(q.get("page", 1)) > 1:
                return self.send(200, [])
            return self.send(200, [{
                "id": 88, "name": "r", "full_name": "o/r", "default_branch": "main",
                "html_url": "http://fake/gt/o/r", "owner": {"login": "o"},
            }])
        if p == "/api/v1/repos/o/r/branches":
            return self.send(200, [{"name": "main"}, {"name": "feature"}])
        if p.endswith("/comments") or "/issues/comments/" in p:
            return self.send(201 if self.command == "POST" else 200, {"id": 557})
        if p.startswith("/api/v1/repos/o/"):
            return self.send(200, {"id": 88, "full_name": "o/r", "default_branch": "main"})
        return self.send(404, {"message": "not found"})


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=18990)
    parser.add_argument("--repo-dir", default=REPO_DIR)
    args = parser.parse_args()
    REPO_DIR = args.repo_dir
    ensure_repo()
    # Port 80 too when it is free: upstream's GitHub App clone URL has no port.
    try:
        threading.Thread(target=ThreadingHTTPServer(("0.0.0.0", 80), Handler).serve_forever, daemon=True).start()
    except OSError:
        pass
    ThreadingHTTPServer(("0.0.0.0", args.port), Handler).serve_forever()
