#!/usr/bin/env python3
"""Local fake of Cloudflare, Doppler, Infisical, Vault and Have I Been Pwned.

    fake_providers.py [port]    default 18191

Everything lives under a provider prefix (`/cloudflare/client/v4`, `/doppler`, `/infisical`,
`/infisical-old`, `/vault`, `/hibp`). `GET /__log` returns the requests since the last
`POST /__reset`, which also forgets DNS records and tunnels. Nothing here contacts anything.
Tokens select behaviour: `cf-good`, `cf-403` (forbidden), `cf-dnsonly` (no tunnel access).
"""
import json
import sys
import threading
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOCK = threading.Lock()
LOG = []
STATE = {"records": {}, "tunnels": {}, "next": 1, "delete_busy": 0}
ZONES = [
    {"id": "zone-example", "name": "Example.com", "account": {"id": "acct-1", "name": "Acme"}},
    {"id": "zone-sub", "name": "sub.example.com", "account": {"id": "acct-1", "name": "Acme"}},
    {"id": "zone-other", "name": "other.test", "account": {"id": "acct-2", "name": "Other"}},
]
DOPPLER = {"DB_PASSWORD": "pa$$word's", "API_KEY": "abc#123", "db-dash": "skip", "COMPLEX": {"a": 1}, "NUM": 5}
VAULT = {"VAULT_ONE": "one$1", "VAULT_JSON": {"x": [1, 2]}, "TAIL": "ends\\"}
HIBP_PWNED = {"5BAA6": "1E4C9B93F3F0682250B6CF8331B7EE68FD8:3861493\r\nFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF:1\r\n"}


def reply(h, code, body, raw=False):
    data = body if raw else json.dumps(body)
    data = data.encode() if isinstance(data, str) else data
    h.send_response(code)
    h.send_header("Content-Type", "text/plain" if raw else "application/json")
    h.send_header("Content-Length", str(len(data)))
    h.end_headers()
    h.wfile.write(data)


def next_id(prefix):
    STATE["next"] += 1
    return f"{prefix}-{STATE['next']}"


def cf_ok(result, **extra):
    return {"success": True, "errors": [], "messages": [], "result": result, **extra}


def cloudflare(h, method, path, q, body, token):
    if token == "cf-403":
        return reply(h, 403, {"success": False, "errors": [{"code": 9109, "message": "Unauthorized"}]})
    if token not in ("cf-good", "cf-dnsonly"):
        return reply(h, 401, {"success": False, "errors": [{"code": 1000, "message": "Invalid API Token"}]})
    p = [x for x in path.split("/") if x]
    if p == ["user", "tokens", "verify"]:
        return reply(h, 200, cf_ok({"id": "t", "status": "active"}))
    if p == ["zones"]:
        page = int(q.get("page", ["1"])[0])
        per = int(q.get("per_page", ["20"])[0])
        start = (page - 1) * per
        return reply(h, 200, cf_ok(ZONES[start:start + per], result_info={"total_pages": (len(ZONES) + per - 1) // per}))
    if len(p) >= 3 and p[0] == "zones" and p[2] == "dns_records":
        zone = p[1]
        if len(p) == 3 and method == "GET":
            rows = [r for r in STATE["records"].values() if r["zone"] == zone]
            if "name" in q:
                rows = [r for r in rows if r["name"] == q["name"][0]]
            if "type" in q:
                rows = [r for r in rows if r["type"] == q["type"][0]]
            return reply(h, 200, cf_ok([{k: v for k, v in r.items() if k != "zone"} for r in rows]))
        if len(p) == 3 and method == "POST":
            rid = next_id("rec")
            STATE["records"][rid] = {"id": rid, "zone": zone, **body}
            return reply(h, 200, cf_ok({"id": rid, **body}))
        rec = STATE["records"].get(p[3]) if len(p) == 4 else None
        if rec is None:
            return reply(h, 404, {"success": False, "errors": [{"message": "Record not found"}]})
        if method == "GET":
            return reply(h, 200, cf_ok({k: v for k, v in rec.items() if k != "zone"}))
        if method == "PATCH":
            rec.update(body)
            return reply(h, 200, cf_ok({k: v for k, v in rec.items() if k != "zone"}))
        if method == "DELETE":
            del STATE["records"][p[3]]
            return reply(h, 200, cf_ok({"id": p[3]}))
    if len(p) >= 3 and p[0] == "accounts" and p[2] == "cfd_tunnel":
        if token == "cf-dnsonly":
            return reply(h, 403, {"success": False, "errors": [{"message": "Forbidden"}]})
        if len(p) == 3 and method == "GET":
            rows = list(STATE["tunnels"].values())
            if "name" in q:
                rows = [t for t in rows if t["name"] == q["name"][0]]
            return reply(h, 200, cf_ok(rows))
        if len(p) == 3 and method == "POST":
            tid = next_id("tunnel")
            STATE["tunnels"][tid] = {"id": tid, "name": body["name"]}
            return reply(h, 200, cf_ok({"id": tid, "token": f"tunnel-token-{tid}"}))
        if len(p) == 5 and p[4] == "token":
            return reply(h, 200, cf_ok(f"tunnel-token-{p[3]}"))
        if len(p) == 5 and p[4] == "configurations":
            return reply(h, 200, cf_ok({"config": body.get("config")}))
        if len(p) == 5 and p[4] == "connections":
            return reply(h, 200, cf_ok({}))
        if len(p) == 4 and method == "DELETE":
            if STATE["delete_busy"] > 0:
                STATE["delete_busy"] -= 1
                return reply(h, 400, {"success": False, "errors": [{"message": "Active connections"}]})
            STATE["tunnels"].pop(p[3], None)
            return reply(h, 200, cf_ok({"id": p[3]}))
    return reply(h, 404, {"success": False, "errors": [{"message": f"no route {method} {path}"}]})


def doppler(h, path, q, token):
    if not token.startswith("dp."):
        return reply(h, 401, {"messages": ["Invalid Auth token"]})
    if "bad" in token:
        return reply(h, 403, {"messages": ["Forbidden by Doppler"]})
    if path == "/v3/me":
        return reply(h, 200, {"workplace": {"name": "x"}})
    if path == "/v3/configs/config/secrets/download":
        if token.startswith("dp.sa.") and "project" not in q:
            return reply(h, 400, {"messages": ["Missing project"]})
        return reply(h, 200, DOPPLER)
    return reply(h, 404, {"messages": [f"no route {path}"]})


def infisical(h, method, path, q, body, token, old=False):
    if path == "/api/v1/auth/universal-auth/login" and method == "POST":
        if body.get("clientSecret") == "good-secret" and body.get("clientId") == "client-1":
            return reply(h, 200, {"accessToken": "access-1", "expiresIn": 7200})
        return reply(h, 401, {"message": "Invalid credentials"})
    if token != "access-1":
        return reply(h, 401, {"message": "Unauthorized"})
    if path == "/api/v4/secrets":
        if old:
            return reply(h, 404, {"message": "Not found"})
    elif not (path == "/api/v3/secrets/raw" and old):
        return reply(h, 404, {"message": f"no route {path}"})
    return reply(h, 200, {
        "imports": [
            {"secrets": [{"secretKey": "SHARED", "secretValue": "first"}, {"secretKey": "IMP", "secretValue": "imp"}]},
            {"secrets": [{"secretKey": "SHARED", "secretValue": "second"}]},
        ],
        "secrets": [{"secretKey": "OWN", "secretValue": "own value"}, {"secretKey": "IMP", "secretValue": "override"}],
    })


def vault(h, path, token, namespace):
    if token != "vault-good":
        return reply(h, 403, {"errors": ["permission denied"]})
    if path == "/v1/auth/token/lookup-self":
        return reply(h, 200, {"data": {"id": "x"}})
    if path == "/v1/secret/data/app/x":
        return reply(h, 200, {"data": {"data": {**VAULT, "NS": namespace or ""}}})
    return reply(h, 404, {"errors": []})


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def handle_any(self):
        url = urllib.parse.urlsplit(self.path)
        q = urllib.parse.parse_qs(url.query, keep_blank_values=True)
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b""
        try:
            body = json.loads(raw) if raw else {}
        except ValueError:
            body = {"_raw": raw.decode("utf-8", "replace")}
        auth = self.headers.get("Authorization") or ""
        token = auth[7:] if auth.lower().startswith("bearer ") else auth
        with LOCK:
            if url.path == "/__reset":
                LOG.clear()
                STATE.update(records={}, tunnels={}, next=1, delete_busy=int(q.get("delete_busy", ["0"])[0]))
                return reply(self, 200, {})
            if url.path == "/__log":
                return reply(self, 200, LOG)
            if url.path == "/__records":
                return reply(self, 200, list(STATE["records"].values()))
            LOG.append({
                "method": self.command,
                "path": url.path,
                "query": {k: v[0] for k, v in sorted(q.items())},
                "body": body,
                "auth": auth,
                "vault_token": self.headers.get("X-Vault-Token"),
                "vault_namespace": self.headers.get("X-Vault-Namespace"),
            })
            parts = url.path.split("/", 2)
            prefix, rest = parts[1], "/" + (parts[2] if len(parts) > 2 else "")
            if prefix == "cloudflare" and rest.startswith("/client/v4"):
                return cloudflare(self, self.command, rest[len("/client/v4"):], q, body, token)
            if prefix == "doppler":
                return doppler(self, rest, q, token)
            if prefix in ("infisical", "infisical-old"):
                return infisical(self, self.command, rest, q, body, token, prefix == "infisical-old")
            if prefix == "vault":
                return vault(self, rest, self.headers.get("X-Vault-Token"), self.headers.get("X-Vault-Namespace"))
            if prefix == "hibp" and rest.startswith("/range/"):
                return reply(self, 200, HIBP_PWNED.get(rest[len("/range/"):].upper(), ""), raw=True)
        return reply(self, 404, {"message": "no such provider"})

    do_GET = do_POST = do_PUT = do_PATCH = do_DELETE = handle_any


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 18191
    ThreadingHTTPServer(("0.0.0.0", port), Handler).serve_forever()
