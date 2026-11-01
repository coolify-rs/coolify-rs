#!/usr/bin/env python3
"""`DeleteServer` at each provider, coolify-rs only, against a local fake. The reference cannot be aimed at a
fake because its provider hosts are fixed, so the expected requests come from `DeleteServer.php`.

    COOLIFY_RS_CLOUD_API_BASE=http://127.0.0.1:18188 scripts/dev-server.sh 18088
    COOLIFY_RS_BASE=http://127.0.0.1:18088 python3 compat/diff/server_delete_providers.py

Rows are tracked by id and removed at the end.
"""
import http.server
import json
import os
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from databases_diff import call, record, results, sql  # noqa: E402

KEY = "hv41morfjv9x0av5kakzdmv8"
ROOT = "acme_owner_root"
seen = []
fail_paths = set()


class Fake(http.server.BaseHTTPRequestHandler):
    def _answer(self):
        seen.append(f"{self.command} {self.path}")
        status = 500 if self.path in fail_paths else 200
        body = b'{"subscription_id":"sub-1","id":1,"data":[]}' if "virtual-machines" in self.path else b"{}"
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(body)

    do_GET = do_DELETE = do_POST = _answer

    def log_message(self, *a):
        pass


server = http.server.ThreadingHTTPServer(("127.0.0.1", 18188), Fake)
threading.Thread(target=server.serve_forever, daemon=True).start()
tokens, servers = [], []


def new_server(name, ip, provider_column, value, token_id):
    st, body = call("rs", "POST", "/api/v1/servers", {"name": name, "ip": ip, "port": 22, "user": "root", "private_key_uuid": KEY, "instant_validate": False}, ROOT)
    assert st == 201, (st, body)
    u = json.loads(body)["uuid"]
    sid = sql(f"select id from servers where uuid='{u}'")
    servers.append(sid)
    sql(f"update servers set {provider_column}='{value}', cloud_provider_token_id={token_id} where id={sid}")
    return u, sid


def token(provider):
    st, body = call("rs", "POST", "/api/v1/cloud-tokens", {"provider": provider, "token": "fake-" + provider, "name": "s2-" + provider}, ROOT)
    assert st == 201, (st, body)
    u = json.loads(body)["uuid"]
    tid = sql(f"select id from cloud_provider_tokens where uuid='{u}'")
    tokens.append(tid)
    return tid


def wait_gone(sid, seconds=20):
    for _ in range(seconds * 2):
        if not sql(f"select id from servers where id={sid}"):
            return True
        time.sleep(0.5)
    return False


try:
    for provider, column, value, expected in [
        ("hetzner", "hetzner_server_id", "123", "DELETE /hetzner/v1/servers/123"),
        ("vultr", "vultr_instance_id", "abc", "DELETE /vultr/v2/instances/abc"),
        ("digitalocean", "digitalocean_droplet_id", "456", "DELETE /digitalocean/v2/droplets/456"),
    ]:
        tid = token(provider)
        u, sid = new_server(f"s2-p-{provider}", f"192.0.2.{10 + len(servers)}", column, value, tid)
        seen.clear()
        st, body = call("rs", "DELETE", f"/api/v1/servers/{u}?delete_from_provider=true", None, ROOT)
        gone = wait_gone(sid)
        record(f"{provider} delete", st == 200 and gone and expected in seen, f"{st} {body} gone={gone} {seen}")

    # A failure: Hetzner still removes the server, the others leave it soft deleted.
    tid = tokens[1]
    u, sid = new_server("s2-p-vultr-fail", "192.0.2.50", "vultr_instance_id", "fails", tid)
    fail_paths.add("/vultr/v2/instances/fails")
    st, body = call("rs", "DELETE", f"/api/v1/servers/{u}?delete_from_provider=true", None, ROOT)
    time.sleep(8)
    row = sql(f"select deleted_at is not null from servers where id={sid}")
    record("vultr failure leaves it soft deleted", st == 200 and row == "t", f"{st} {body} {row}")
    sql(f"delete from standalone_dockers where server_id={sid}")
    sql(f"delete from server_settings where server_id={sid}")
    sql(f"delete from servers where id={sid}")

    tid = tokens[0]
    u, sid = new_server("s2-p-hetzner-fail", "192.0.2.51", "hetzner_server_id", "999", tid)
    fail_paths.add("/hetzner/v1/servers/999")
    st, body = call("rs", "DELETE", f"/api/v1/servers/{u}?delete_from_provider=true", None, ROOT)
    record("hetzner failure still removes it", st == 200 and wait_gone(sid, 40), f"{st} {body}")
finally:
    for sid in servers:
        for q in ("standalone_dockers", "swarm_dockers", "server_settings"):
            sql(f"delete from {q} where server_id={sid}")
        sql(f"delete from servers where id={sid}")
    for t in tokens:
        sql(f"delete from cloud_provider_tokens where id={t}")
print(f"{sum(results)}/{len(results)}")
sys.exit(0 if all(results) else 1)
