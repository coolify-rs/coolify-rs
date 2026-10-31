#!/usr/bin/env python3
"""Differential check of servers: upstream reference vs coolify-rs.

    COOLIFY_RS_BASE=http://127.0.0.1:18084 servers_diff.py api
    COOLIFY_RS_BASE=http://127.0.0.1:18084 servers_diff.py cloud      (coolify-rs started with COOLIFY_RS_CLOUD_API_BASE=http://127.0.0.1:18184)
    COOLIFY_RS_BASE=http://127.0.0.1:18084 servers_diff.py jobs --vm-a NAME --vm-b NAME

`api` creates one throwaway server per side and compares responses (status, key order, values) and
rows of every ported route, for several tokens. `cloud` runs the provider routes: the reference inside a
tinker process with `Http::fake`, coolify-rs against a local fake with the same answers, and compares the
responses, the rows and the requests sent to the providers. `jobs` needs two OrbStack VMs without Docker,
one per side: it installs them through the API and compares commands' results: files, containers, rows.
Everything it creates is deleted at the end. Exit status 1 when anything differs.
"""
import argparse
import base64
import json
import os
import re
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
TOKENS = json.load(open(os.environ.get("COOLIFY_RS_TOKENS_FILE") or os.path.join(ROOT, "compat/reference/secrets/tokens.json")))
BASES = {"ref": "http://127.0.0.1:18000", "rs": os.environ.get("COOLIFY_RS_BASE", "http://127.0.0.1:18084")}
FAKE_PORT = int(os.environ.get("FAKE_PORT", "18184"))
KEY_UUID = "hv41morfjv9x0av5kakzdmv8"  # "orbstack e2e" key of the Acme team
results = []


def call(side, method, path, body=None, tok="acme_owner_root", raw=None, headers=None):
    data = raw if raw is not None else (json.dumps(body).encode() if body is not None else None)
    for _ in range(120):
        r = urllib.request.Request(BASES[side] + path, data=data, method=method)
        if tok:
            r.add_header("Authorization", "Bearer " + TOKENS[tok])
        r.add_header("Accept", "application/json")
        if data is not None:
            r.add_header("Content-Type", "application/json")
        for k, v in (headers or {}).items():
            r.add_header(k, v)
        try:
            resp = urllib.request.urlopen(r, timeout=120)
            return resp.status, resp.read().decode()
        except urllib.error.HTTPError as e:
            if e.code == 429:
                time.sleep(max(5, int(e.headers.get("Retry-After", "5"))))
                continue
            return e.code, e.read().decode()
    raise SystemExit("rate limited")


def sql(query):
    out = subprocess.run(
        ["docker", "exec", "coolify-rs-reference-postgres-1", "psql", "-U", "coolify", "-d", "coolify", "-At", "-c", query],
        check=True, capture_output=True, text=True,
    ).stdout
    return out.strip()


def record(name, ok, detail=""):
    results.append(ok)
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"\n  {detail}"[:3000]))


class Sides:
    """Names per side, so that responses compare after replacing them."""

    def __init__(self):
        self.vars = {"ref": {}, "rs": {}}

    def set(self, name, ref, rs):
        self.vars["ref"][name] = ref
        self.vars["rs"]["name" if False else name] = rs

    def fmt(self, side, text):
        for k, v in self.vars[side].items():
            text = text.replace("{" + k + "}", str(v))
        return text

    def norm(self, side, text):
        if BASES[side]:
            text = text.replace(BASES[side].replace("http://", "").replace("/", r"\/"), "HOST")
            text = text.replace(BASES[side].replace("http://", ""), "HOST")
        for k, v in sorted(self.vars[side].items(), key=lambda kv: -len(str(kv[1]))):
            if len(str(v)) >= 3 and not str(v).isdigit():
                text = text.replace(str(v), "<" + k + ">")
        text = re.sub(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+Z", "<ts>", text)
        text = re.sub(r"\d{4}-\d\d-\d\d \d\d:\d\d:\d\d", "<ts>", text)
        text = re.sub(r'"uuid":"[a-z0-9]{24}"', '"uuid":"U"', text)
        text = re.sub(r'"sentinel_token":"[^"]*"', '"sentinel_token":"T"', text)
        text = re.sub(r'"id":\d+', '"id":0', text)
        text = re.sub(r'"server_id":\d+', '"server_id":0', text)
        return text


S = Sides()


def same(name, method, path, body=None, tok="acme_owner_root", headers=None, raw=None):
    results_ = {}
    for side in ("ref", "rs"):
        b = None
        if body is not None:
            b = json.loads(S.fmt(side, json.dumps(body)))
        results_[side] = call(side, method, S.fmt(side, path), b, tok, raw=raw, headers=headers)
    a = (results_["ref"][0], S.norm("ref", results_["ref"][1]))
    c = (results_["rs"][0], S.norm("rs", results_["rs"][1]))
    record(f"{name} [{tok}]", a == c, first_difference(a, c))
    return results_


def first_difference(a, c):
    """Where two responses differ: the first differing JSON path, or both bodies."""
    try:
        x, y = json.loads(a[1]), json.loads(c[1])
    except ValueError:
        return f"ref {a}\n  rs  {c}"
    if a[0] != c[0]:
        return f"status {a[0]} / {c[0]}\n  ref {a[1][:800]}\n  rs  {c[1][:800]}"

    def walk(u, v, path):
        if type(u) is not type(v):
            return f"{path}: {json.dumps(u)[:300]} | {json.dumps(v)[:300]}"
        if isinstance(u, dict):
            if list(u) != list(v):
                missing = [k for k in u if k not in v]
                extra = [k for k in v if k not in u]
                if missing or extra:
                    return f"{path}: only ref {missing}, only rs {extra}"
                return f"{path}: key order\n  ref {list(u)}\n  rs  {list(v)}"
            for k in u:
                d = walk(u[k], v[k], f"{path}.{k}")
                if d:
                    return d
        elif isinstance(u, list):
            if len(u) != len(v):
                return f"{path}: length {len(u)} / {len(v)}"
            for i, (e, f) in enumerate(zip(u, v)):
                d = walk(e, f, f"{path}[{i}]")
                if d:
                    return d
        elif u != v:
            return f"{path}: {json.dumps(u)[:300]} | {json.dumps(v)[:300]}"
        return None

    return walk(x, y, "$") or f"raw bytes differ\n  ref {a[1][:600]}\n  rs  {c[1][:600]}"


def row(table, where):
    r = sql(f"select row_to_json(t)::text from {table} t where {where} limit 1")
    return json.loads(r) if r else None


def rows_same(name, table, where_ref, where_rs, ignore=()):
    a, b = row(table, where_ref), row(table, where_rs)
    drop = {"id", "uuid", "created_at", "updated_at", "server_id", "name", *ignore}
    x = {k: v for k, v in (a or {}).items() if k not in drop}
    y = {k: v for k, v in (b or {}).items() if k not in drop}
    # A time on both sides, or on neither: `StartSentinel` runs asynchronously on each.
    for t in (x, y):
        if "sentinel_waiting_since" in t:
            t["sentinel_waiting_since"] = t["sentinel_waiting_since"] is not None
    bad = {k: (x.get(k), y.get(k)) for k in set(x) | set(y) if x.get(k) != y.get(k)}
    record(f"row {name}", not bad and a is not None, json.dumps(bad)[:1500])


def create_server(side, name, ip, **extra):
    st, body = call(side, "POST", "/api/v1/servers", {"name": name, "ip": ip, "private_key_uuid": KEY_UUID, **extra})
    if st != 201:
        raise SystemExit(f"cannot create a server on {side}: {st} {body}")
    return json.loads(body)["uuid"]


def delete_server(uuid):
    sid = sql(f"select id from servers where uuid='{uuid}'")
    if not sid:
        return
    for q in (
        f"delete from docker_cleanup_executions where server_id={sid}",
        f"delete from shared_environment_variables where server_id={sid}",
        f"delete from standalone_dockers where server_id={sid}",
        f"delete from swarm_dockers where server_id={sid}",
        f"delete from ssl_certificates where server_id={sid}",
        f"delete from server_settings where server_id={sid}",
        f"delete from servers where id={sid}",
    ):
        sql(q)


ALL_TOKENS = ["acme_owner_root", "acme_owner_read", "acme_owner_write", "acme_member_read", "carol_root"]


def api():
    a = create_server("ref", "diff-srv-a", "198.51.100.71")
    b = create_server("ref", "diff-srv-b", "198.51.100.72")
    S.set("s", a, b)
    S.vars["ref"]["sname"] = "diff-srv-a"
    S.vars["rs"]["sname"] = "diff-srv-b"
    S.vars["ref"]["sip"] = "198.51.100.71"
    S.vars["rs"]["sip"] = "198.51.100.72"
    for t in (a, b):
        sid = sql(f"select id from servers where uuid='{t}'")
        S.vars["ref" if t == a else "rs"]["sid"] = sid
    created = []
    try:
        for tok in ALL_TOKENS:
            for sub in ("sentinel", "docker-cleanup", "docker-cleanup/executions", "log-drains", "cloudflare-tunnel",
                        "proxy", "envs", "destinations", "resources", "domains", "registries", "transfer"):
                if sub == "sentinel" and tok != "acme_member_read":
                    # The tokens differ per side; compared without the secret below.
                    pass
                r = {}
                st = {}
                for side in ("ref", "rs"):
                    st[side] = call(side, "GET", S.fmt(side, f"/api/v1/servers/{{s}}/{sub}"), tok=tok)
                    r[side] = re.sub(r'"sentinel_token":"[^"]*"', '"sentinel_token":"T"', S.norm(side, st[side][1]))
                record(f"GET {sub} [{tok}]", (st["ref"][0], r["ref"]) == (st["rs"][0], r["rs"]),
                       f"ref {st['ref'][0]} {r['ref']}\n  rs  {st['rs'][0]} {r['rs']}")
            same("GET missing server sentinel", "GET", "/api/v1/servers/nope/sentinel", tok=tok)
            same("GET destinations by missing server", "GET", "/api/v1/servers/nope/destinations", tok=tok)
            same("GET destination missing", "GET", "/api/v1/destinations/nope", tok=tok)
            same("GET cloud tokens", "GET", "/api/v1/cloud-tokens", tok=tok)
            same("GET cloud-init", "GET", "/api/v1/cloud-init-scripts", tok=tok)
        resources_on_carol()
        # Sentinel.
        same("PATCH sentinel invalid", "PATCH", "/api/v1/servers/{s}/sentinel",
             {"is_metrics_enabled": "x", "sentinel_token": "a b", "sentinel_metrics_refresh_rate_seconds": 0,
              "sentinel_push_interval_seconds": 5, "sentinel_custom_url": "nope", "traffic_topn": 2000,
              "traffic_ip_mode": "weird", "foo": 1})
        same("PATCH sentinel no body", "PATCH", "/api/v1/servers/{s}/sentinel", raw=b"")
        same("PATCH sentinel write token", "PATCH", "/api/v1/servers/{s}/sentinel", {"is_metrics_enabled": True}, tok="acme_owner_write")
        same("PATCH sentinel read token", "PATCH", "/api/v1/servers/{s}/sentinel", {"is_metrics_enabled": True}, tok="acme_owner_read")
        same("PATCH sentinel", "PATCH", "/api/v1/servers/{s}/sentinel",
             {"is_metrics_enabled": True, "is_sentinel_debug_enabled": True, "sentinel_token": "tok.en-1",
              "sentinel_metrics_refresh_rate_seconds": 20, "sentinel_metrics_history_days": 3,
              "sentinel_push_interval_seconds": 30, "sentinel_custom_url": "http://example.com:8000", "traffic_topn": 10,
              "traffic_sample_threshold": 2, "traffic_retention_1h_days": 5, "traffic_retention_1d_days": 6,
              "is_geoip_enabled": False, "geoip_refresh_days": 9, "geoip_maxmind_license_key": "lic",
              "traffic_ip_mode": "off"})
        # The queued `StartSentinel` reaches `sentinel_waiting_since` only after `StopSentinel`'s ssh attempts to the
        # unreachable address: about 70 seconds on the reference, at once where ssh-via-orb refuses the host. The rows are
        # compared after both.
        for side in ("ref", "rs"):
            wait_for(f"{side} sentinel start", lambda: sql(f"select sentinel_waiting_since is not null from servers where id={S.vars[side]['sid']}") == "t", timeout=180, step=3)
        rows_same("settings after sentinel", "server_settings", f"server_id={S.vars['ref']['sid']}",
                  f"server_id={S.vars['rs']['sid']}", ignore=("sentinel_token", "geoip_maxmind_license_key"))
        # Docker cleanup.
        same("PATCH cleanup invalid", "PATCH", "/api/v1/servers/{s}/docker-cleanup",
             {"docker_cleanup_frequency": 5, "docker_cleanup_threshold": 100, "force_docker_cleanup": "x", "foo": 1})
        same("PATCH cleanup bad cron", "PATCH", "/api/v1/servers/{s}/docker-cleanup", {"docker_cleanup_frequency": "nope"})
        same("PATCH cleanup volumes without deploy", "PATCH", "/api/v1/servers/{s}/docker-cleanup",
             {"delete_unused_volumes": True}, tok="acme_owner_write")
        same("PATCH cleanup", "PATCH", "/api/v1/servers/{s}/docker-cleanup",
             {"docker_cleanup_frequency": "daily", "docker_cleanup_threshold": 70, "force_docker_cleanup": False,
              "delete_unused_volumes": True, "delete_unused_networks": True, "disable_application_image_retention": True})
        rows_same("settings after cleanup", "server_settings", f"server_id={S.vars['ref']['sid']}",
                  f"server_id={S.vars['rs']['sid']}", ignore=("sentinel_token", "geoip_maxmind_license_key"))
        same("POST cleanup run invalid", "POST", "/api/v1/servers/{s}/docker-cleanup/run", {"delete_unused_volumes": "x", "foo": 1})
        same("POST cleanup run read token", "POST", "/api/v1/servers/{s}/docker-cleanup/run", tok="acme_owner_read")
        same("POST cleanup run write only", "POST", "/api/v1/servers/{s}/docker-cleanup/run", tok="acme_owner_write")
        same("POST cleanup run", "POST", "/api/v1/servers/{s}/docker-cleanup/run", {"delete_unused_volumes": False})
        for side in ("ref", "rs"):
            wait_for(f"{side} cleanup row", lambda: sql(f"select count(*) from docker_cleanup_executions where server_id={S.vars[side]['sid']} and finished_at is not null") == "1", timeout=120, step=2)
        same("GET executions", "GET", "/api/v1/servers/{s}/docker-cleanup/executions")
        # Log drains.
        same("PATCH drains invalid", "PATCH", "/api/v1/servers/{s}/log-drains",
             {"is_logdrain_newrelic_enabled": "x", "logdrain_newrelic_license_key": "a b", "logdrain_newrelic_base_uri": "nope",
              "logdrain_axiom_dataset_name": "a b", "logdrain_custom_config": "[INPUT]\n  Name exec",
              "logdrain_custom_config_parser": "[OUTPUT]", "logdrain_cloudwatch_region": "x",
              "logdrain_cloudwatch_group": "a b", "logdrain_cloudwatch_stream_prefix": "a b", "foo": 1})
        same("PATCH drains newrelic missing", "PATCH", "/api/v1/servers/{s}/log-drains", {"is_logdrain_newrelic_enabled": True})
        same("PATCH drains axiom missing", "PATCH", "/api/v1/servers/{s}/log-drains", {"is_logdrain_axiom_enabled": True})
        same("PATCH drains custom missing", "PATCH", "/api/v1/servers/{s}/log-drains", {"is_logdrain_custom_enabled": True})
        same("PATCH drains cloudwatch missing", "PATCH", "/api/v1/servers/{s}/log-drains", {"is_logdrain_cloudwatch_enabled": True})
        same("PATCH drains cloudwatch with other", "PATCH", "/api/v1/servers/{s}/log-drains",
             {"is_logdrain_cloudwatch_enabled": True, "is_logdrain_custom_enabled": True, "logdrain_custom_config": "[SERVICE]\n Flush 5"})
        same("PATCH drains custom", "PATCH", "/api/v1/servers/{s}/log-drains",
             {"is_logdrain_custom_enabled": True, "logdrain_custom_config": "[SERVICE]\n Flush 5", "logdrain_custom_config_parser": "[PARSER]\n Name x"})
        same("PATCH drains newrelic", "PATCH", "/api/v1/servers/{s}/log-drains",
             {"is_logdrain_custom_enabled": False, "is_logdrain_newrelic_enabled": True, "logdrain_newrelic_license_key": "abc_1",
              "logdrain_newrelic_base_uri": "https://log-api.newrelic.com/log/v1"})
        same("PATCH drains off", "PATCH", "/api/v1/servers/{s}/log-drains",
             {"is_logdrain_newrelic_enabled": False, "logdrain_cloudwatch_region": "eu-west-1", "logdrain_cloudwatch_group": "/a/b",
              "logdrain_cloudwatch_stream_prefix": "x"})
        rows_same("settings after drains", "server_settings", f"server_id={S.vars['ref']['sid']}",
                  f"server_id={S.vars['rs']['sid']}", ignore=("sentinel_token", "geoip_maxmind_license_key", "logdrain_custom_config"))
        # Cloudflare tunnel.
        same("PATCH tunnel invalid", "PATCH", "/api/v1/servers/{s}/cloudflare-tunnel", {"is_cloudflare_tunnel": "x", "foo": 1})
        same("PATCH tunnel", "PATCH", "/api/v1/servers/{s}/cloudflare-tunnel", {"is_cloudflare_tunnel": True})
        same("POST tunnel disable", "POST", "/api/v1/servers/{s}/cloudflare-tunnel/disable")
        same("POST tunnel enable", "POST", "/api/v1/servers/{s}/cloudflare-tunnel/enable")
        same("PATCH tunnel off", "PATCH", "/api/v1/servers/{s}/cloudflare-tunnel", {"is_cloudflare_tunnel": False})
        rows_same("settings after tunnel", "server_settings", f"server_id={S.vars['ref']['sid']}",
                  f"server_id={S.vars['rs']['sid']}", ignore=("sentinel_token", "geoip_maxmind_license_key", "logdrain_custom_config"))
        same("tunnel on localhost", "PATCH", "/api/v1/servers/fjzwuetbopl28gadfrj6ozsr/cloudflare-tunnel", {"is_cloudflare_tunnel": True}, tok="root_root")
        # Server shared variables.
        same("POST env invalid", "POST", "/api/v1/servers/{s}/envs", {"key": "1 bad", "is_literal": "x", "comment": "x" * 300, "foo": 1})
        same("POST env no key", "POST", "/api/v1/servers/{s}/envs", {"value": "v"})
        for side in ("ref", "rs"):
            pass
        st = same("POST env", "POST", "/api/v1/servers/{s}/envs", {"key": "DIFF_VAR", "value": "secret", "is_literal": True, "comment": "c"})
        if st["ref"][0] == 201 and st["rs"][0] == 201:
            S.set("e", json.loads(st["ref"][1])["id"], json.loads(st["rs"][1])["id"])
        same("POST env dup", "POST", "/api/v1/servers/{s}/envs", {"key": "DIFF_VAR", "value": "x"})
        same("GET envs", "GET", "/api/v1/servers/{s}/envs")
        same("GET envs read", "GET", "/api/v1/servers/{s}/envs", tok="acme_owner_read")
        same("PATCH env missing", "PATCH", "/api/v1/servers/{s}/envs/999999", {"value": "x"})
        same("PATCH env empty", "PATCH", "/api/v1/servers/{s}/envs/{e}", raw=b"{}")
        same("PATCH env key taken", "PATCH", "/api/v1/servers/{s}/envs/{e}", {"key": "COOLIFY_SERVER_UUID"})
        same("PATCH env", "PATCH", "/api/v1/servers/{s}/envs/{e}", {"key": "DIFF_VAR2", "value": "other", "is_multiline": True, "comment": None})
        rows_same("shared variable", "shared_environment_variables",
                  f"server_id={S.vars['ref']['sid']} and key='DIFF_VAR2'", f"server_id={S.vars['rs']['sid']} and key='DIFF_VAR2'",
                  ignore=("value",))
        same("DELETE env", "DELETE", "/api/v1/servers/{s}/envs/{e}")
        same("DELETE env again", "DELETE", "/api/v1/servers/{s}/envs/{e}")
        # Destinations on a server that is not functional.
        same("POST destination invalid", "POST", "/api/v1/servers/{s}/destinations", {"network": "-bad", "type": "nope", "name": 5, "foo": 1})
        same("POST destination reserved", "POST", "/api/v1/servers/{s}/destinations", {"network": "Bridge"})
        same("POST destination wrong type", "POST", "/api/v1/servers/{s}/destinations", {"network": "n1", "type": "swarm"})
        same("POST destination null type", "POST", "/api/v1/servers/{s}/destinations", {"network": "n1", "type": None})
        same("POST destination existing", "POST", "/api/v1/servers/{s}/destinations", {"network": "coolify"})
        same("POST destination not functional", "POST", "/api/v1/servers/{s}/destinations", {"network": "n1"})
        same("POST destination member", "POST", "/api/v1/servers/{s}/destinations", {"network": "n1"}, tok="acme_member_read")
        # A swarm destination does not touch the server.
        for side, sid in (("ref", S.vars["ref"]["sid"]), ("rs", S.vars["rs"]["sid"])):
            sql(f"update server_settings set is_swarm_manager=true where server_id={sid}")
        same("POST swarm destination", "POST", "/api/v1/servers/{s}/destinations", {"network": "swarm-n", "name": "<b>Swarm</b>"})
        same("POST swarm dup", "POST", "/api/v1/servers/{s}/destinations", {"network": "swarm-n"})
        uu = {}
        for side in ("ref", "rs"):
            uu[side] = sql(f"select uuid from swarm_dockers where server_id={S.vars[side]['sid']}")
        S.set("d", uu["ref"], uu["rs"])
        same("GET destination", "GET", "/api/v1/destinations/{d}")
        same("GET destinations by server", "GET", "/api/v1/servers/{s}/destinations")
        same("PATCH destination invalid", "PATCH", "/api/v1/destinations/{d}", {"name": None, "foo": 1})
        same("PATCH destination", "PATCH", "/api/v1/destinations/{d}", {"name": "renamed"})
        same("PATCH destination member", "PATCH", "/api/v1/destinations/{d}", {"name": "x"}, tok="acme_member_read")
        same("DELETE destination member", "DELETE", "/api/v1/destinations/{d}", tok="acme_member_read")
        same("DELETE destination", "DELETE", "/api/v1/destinations/{d}")
        same("DELETE destination again", "DELETE", "/api/v1/destinations/{d}")
        for side, sid in (("ref", S.vars["ref"]["sid"]), ("rs", S.vars["rs"]["sid"])):
            sql(f"update server_settings set is_swarm_manager=false where server_id={sid}")
        # Proxy, on a server that is not functional.
        same("PATCH proxy invalid", "PATCH", "/api/v1/servers/{s}/proxy",
             {"redirect_enabled": "x", "redirect_url": "ftp://x", "traefik_certificate_resolver": "a b", "proxy_type": 5, "foo": 1})
        same("PATCH proxy private url", "PATCH", "/api/v1/servers/{s}/proxy", {"redirect_url": "http://127.0.0.1/x"})
        same("PATCH proxy bad type", "PATCH", "/api/v1/servers/{s}/proxy", {"proxy_type": "nope"})
        same("PATCH proxy type without deploy", "PATCH", "/api/v1/servers/{s}/proxy", {"proxy_type": "caddy"}, tok="acme_owner_write")
        same("PATCH proxy", "PATCH", "/api/v1/servers/{s}/proxy",
             {"redirect_enabled": False, "redirect_url": "https://1.1.1.1/x", "proxy_error_page": "<h1>x</h1>",
              "generate_exact_labels": True, "traefik_certificate_resolver": "myres"})
        rows_same("server after proxy", "servers", f"id={S.vars['ref']['sid']}", f"id={S.vars['rs']['sid']}",
                  ignore=("ip", "ip_previous", "sentinel_updated_at"))
        same("PATCH proxy clear", "PATCH", "/api/v1/servers/{s}/proxy", {"redirect_url": None, "proxy_error_page": "", "traefik_certificate_resolver": None})
        same("PATCH proxy type", "PATCH", "/api/v1/servers/{s}/proxy", {"proxy_type": "CADDY"})
        rows_same("server after proxy type", "servers", f"id={S.vars['ref']['sid']}", f"id={S.vars['rs']['sid']}",
                  ignore=("ip", "ip_previous", "sentinel_updated_at"))
        same("PUT proxy config without deploy", "PUT", "/api/v1/servers/{s}/proxy/configuration", {"configuration": "x"}, tok="acme_owner_write")
        same("PUT proxy config invalid", "PUT", "/api/v1/servers/{s}/proxy/configuration", {"configuration": 5, "foo": 1})
        same("PUT proxy config blank b64", "PUT", "/api/v1/servers/{s}/proxy/configuration",
             {"configuration": base64.b64encode(b"   ").decode()})
        same("PUT proxy config not functional", "PUT", "/api/v1/servers/{s}/proxy/configuration", {"configuration": "a: 1"})
        same("POST proxy restart not functional", "POST", "/api/v1/servers/{s}/proxy/restart")
        same("POST proxy restart read", "POST", "/api/v1/servers/{s}/proxy/restart", tok="acme_owner_read")
        same("POST proxy restart member", "POST", "/api/v1/servers/{s}/proxy/restart", tok="acme_member_read")
        # Registries on a server that is not functional.
        same("POST registry invalid", "POST", "/api/v1/servers/{s}/registries",
             {"registry": "UP PER!", "username": "a b", "password": "", "foo": 1})
        same("POST registry not reachable", "POST", "/api/v1/servers/{s}/registries",
             {"registry": "https://GHCR.io/v2", "username": "u", "password": "p"})
        same("POST registry check invalid", "POST", "/api/v1/servers/{s}/registries/a%20b/check")
        same("POST registry check", "POST", "/api/v1/servers/{s}/registries/ghcr.io/check")
        same("DELETE registry", "DELETE", "/api/v1/servers/{s}/registries/ghcr.io")
        same("GET registries read", "GET", "/api/v1/servers/{s}/registries", tok="acme_owner_read")
        same("GET registries sensitive", "GET", "/api/v1/servers/{s}/registries", tok="root_read")
        # Transfer status.
        same("GET transfer", "GET", "/api/v1/servers/{s}/transfer")
        same("GET server with resources", "GET", "/api/v1/servers/{s}?resources=1", tok="acme_owner_read")
        # Validation answers.
        same("validate install without deploy", "POST", "/api/v1/servers/{s}/validate", {"install": True}, tok="acme_owner_write")
        same("validate invalid", "POST", "/api/v1/servers/{s}/validate", {"install": "x"})
        # Cloud-init scripts.
        same("POST script invalid", "POST", "/api/v1/cloud-init-scripts", {"name": None, "script": "a: [", "foo": 1})
        same("POST script bad yaml", "POST", "/api/v1/cloud-init-scripts", {"name": "x", "script": "#cloud-config\na: ["})
        st = same("POST script", "POST", "/api/v1/cloud-init-scripts", {"name": "diff-script", "script": "#!/bin/sh\necho hi"})
        if st["ref"][0] == 201 and st["rs"][0] == 201:
            S.set("c", json.loads(st["ref"][1])["uuid"], json.loads(st["rs"][1])["uuid"])
            created.extend([S.vars["ref"]["c"], S.vars["rs"]["c"]])
        same("GET script", "GET", "/api/v1/cloud-init-scripts/{c}")
        same("GET script read", "GET", "/api/v1/cloud-init-scripts/{c}", tok="acme_owner_read")
        same("GET script member", "GET", "/api/v1/cloud-init-scripts/{c}", tok="acme_member_read")
        same("GET script missing", "GET", "/api/v1/cloud-init-scripts/nope")
        same("PATCH script empty", "PATCH", "/api/v1/cloud-init-scripts/{c}", raw=b"{}")
        same("PATCH script", "PATCH", "/api/v1/cloud-init-scripts/{c}", {"name": "diff-script2", "script": "a: 1"})
        same("PATCH script extra", "PATCH", "/api/v1/cloud-init-scripts/{c}", {"foo": 1})
        same("DELETE script", "DELETE", "/api/v1/cloud-init-scripts/{c}")
        same("DELETE script again", "DELETE", "/api/v1/cloud-init-scripts/{c}")
        # Cloud tokens without a provider call.
        same("POST cloud token invalid", "POST", "/api/v1/cloud-tokens", {"provider": "aws", "token": 5, "foo": 1})
        same("GET cloud token missing", "GET", "/api/v1/cloud-tokens/nope")
        same("PATCH cloud token missing", "PATCH", "/api/v1/cloud-tokens/nope", {"name": "x"})
        same("PATCH cloud token invalid", "PATCH", "/api/v1/cloud-tokens/nope", {"foo": 1})
        same("DELETE cloud token missing", "DELETE", "/api/v1/cloud-tokens/nope")
        same("POST validate token missing", "POST", "/api/v1/cloud-tokens/nope/validate")
        # Provider routes without a token.
        for p in ("hetzner/locations", "hetzner/server-types", "hetzner/images", "hetzner/ssh-keys", "hetzner/firewalls",
                  "hetzner/networks", "vultr/regions", "vultr/plans", "vultr/os", "vultr/ssh-keys", "digitalocean/regions",
                  "digitalocean/sizes", "digitalocean/images", "digitalocean/ssh-keys", "hostinger/data-centers",
                  "hostinger/catalog", "hostinger/templates", "hostinger/ssh-keys", "hostinger/post-install-scripts"):
            same(f"GET {p} no token", "GET", f"/api/v1/{p}")
            same(f"GET {p} unknown token", "GET", f"/api/v1/{p}?cloud_provider_token_uuid=nope")
        for p, body in (("hetzner", {"foo": 1, "image": "x"}), ("vultr", {"foo": 1, "os_id": "x"}),
                        ("digitalocean", {"foo": 1}), ("hostinger", {"foo": 1, "data_center_id": "x"})):
            same(f"POST servers/{p} invalid", "POST", f"/api/v1/servers/{p}", body)
            same(f"POST servers/{p} member", "POST", f"/api/v1/servers/{p}", body, tok="acme_member_read")
        # Sentinel push.
        tokens = {}
        for side in ("ref", "rs"):
            cipher = call("ref", "GET", f"/api/v1/servers/{S.vars[side]['s']}/sentinel", tok="acme_owner_root")
            tokens[side] = json.loads(cipher[1]).get("sentinel_token")
        def push(name, auth, body, raw=None):
            out = {}
            for side in ("ref", "rs"):
                h = {"Authorization": auth(side)} if auth else {}
                r = urllib.request.Request(BASES[side] + "/api/v1/sentinel/push",
                                           data=raw if raw is not None else json.dumps(body).encode(), method="POST")
                r.add_header("Content-Type", "application/json")
                r.add_header("Accept", "application/json")
                for k, v in h.items():
                    r.add_header(k, v)
                try:
                    resp = urllib.request.urlopen(r)
                    out[side] = (resp.status, resp.read().decode())
                except urllib.error.HTTPError as e:
                    out[side] = (e.code, e.read().decode())
            record(f"push {name}", out["ref"] == out["rs"], str(out))
        push("no token", None, {"containers": []})
        push("garbage token", lambda s: "Bearer nope", {"containers": []})
        push("not functional", lambda s: "Bearer " + tokens[s], {"containers": []})
    finally:
        delete_server(a)
        delete_server(b)
        for c in created:
            sql(f"delete from cloud_init_scripts where uuid='{c}'")


def resources_on_carol():
    """`/resources` on a team with an application, a database and a service: one per side is not needed, it is a read."""
    tok = "carol_root"
    st, b = call("ref", "POST", "/api/v1/security/keys", {"name": "diff-carol", "private_key": PEM}, tok=tok)
    key = json.loads(b)["uuid"]
    st, b = call("ref", "POST", "/api/v1/servers", {"name": "diff-carol", "ip": "198.51.100.73", "private_key_uuid": key}, tok=tok)
    server = json.loads(b)["uuid"]
    st, b = call("ref", "POST", "/api/v1/projects", {"name": "diff-carol"}, tok=tok)
    project = json.loads(b)["uuid"]
    try:
        st, b = call("ref", "POST", "/api/v1/applications/dockerimage",
                     {"project_uuid": project, "server_uuid": server, "environment_name": "production",
                      "docker_registry_image_name": "nginx", "ports_exposes": "80", "name": "diff-app"}, tok=tok)
        record("carol application created", st == 201, b)
        st, b = call("ref", "POST", "/api/v1/databases/redis",
                     {"project_uuid": project, "server_uuid": server, "environment_name": "production", "name": "diff-db"}, tok=tok)
        record("carol database created", st == 201, b)
        compose = base64.b64encode(b"services:\n  web:\n    image: nginx\n    environment:\n      - SERVICE_FQDN_WEB_80\n  db:\n    image: postgres:16\n").decode()
        st, b = call("ref", "POST", "/api/v1/services",
                     {"project_uuid": project, "server_uuid": server, "environment_name": "production",
                      "docker_compose_raw": compose, "name": "diff-svc"}, tok=tok)
        record("carol service created", st == 201, b)
        service = json.loads(b)["uuid"]
        S.vars["ref"]["cs"] = S.vars["rs"]["cs"] = server
        same("GET resources", "GET", "/api/v1/resources", tok=tok)
        same("GET server resources", "GET", "/api/v1/servers/{cs}/resources", tok=tok)
        same("GET server with resources", "GET", "/api/v1/servers/{cs}?resources=1", tok=tok)
        same("GET server domains", "GET", "/api/v1/servers/{cs}/domains", tok=tok)
        same("GET registries with images", "GET", "/api/v1/servers/{cs}/registries", tok=tok)
        same("DELETE destination attached", "DELETE", "/api/v1/destinations/" + sql(
            f"select uuid from standalone_dockers where server_id=(select id from servers where uuid='{server}')"), tok=tok)
        st, b = call("ref", "DELETE", f"/api/v1/services/{service}?delete_connected_networks=false&docker_cleanup=false", tok=tok)
        record("carol service deleted", st == 200, b)
    finally:
        sid = sql(f"select id from servers where uuid='{server}'")
        pid = sql(f"select id from projects where uuid='{project}'")
        for q in (f"delete from service_applications where service_id in (select id from services where environment_id in (select id from environments where project_id={pid}))",
                  f"delete from service_databases where service_id in (select id from services where environment_id in (select id from environments where project_id={pid}))",
                  f"delete from services where environment_id in (select id from environments where project_id={pid})",
                  f"delete from standalone_redis where environment_id in (select id from environments where project_id={pid})",
                  f"delete from environment_variables where resourceable_type='App\\Models\\Application' and resourceable_id in "
                  f"(select id from applications where environment_id in (select id from environments where project_id={pid}))",
                  f"delete from application_settings where application_id in (select id from applications where environment_id in (select id from environments where project_id={pid}))",
                  f"delete from applications where environment_id in (select id from environments where project_id={pid})",
                  f"delete from environments where project_id={pid}", f"delete from project_settings where project_id={pid}", f"delete from projects where id={pid}"):
            sql(q)
        delete_server(server)
        sql(f"delete from private_keys where uuid='{key}'")


PEM = subprocess.run(
    ["python3", "-c", "import subprocess,tempfile,os;d=tempfile.mkdtemp();p=os.path.join(d,'k');subprocess.run(['ssh-keygen','-q','-t','ed25519','-N','','-f',p]);print(open(p).read())"],
    capture_output=True, text=True).stdout


# ── Cloud providers ─────────────────────────────────────────────────────

PUBLIC_KEY_FP = "__FP__"
FAKE = {
    ("GET", "/hetzner/v1/locations"): {"locations": [{"id": 1, "name": "fsn1"}], "meta": {"pagination": {"next_page": None}}},
    ("GET", "/hetzner/v1/server_types"): {"server_types": [{"id": 1, "name": "cx11"}, {"id": 2, "name": "old", "deprecated": True}], "meta": {"pagination": {"next_page": None}}},
    ("GET", "/hetzner/v1/images"): {"images": [{"id": 5, "type": "system"}, {"id": 6, "type": "snapshot"}, {"id": 7, "type": "system", "deprecated": True}], "meta": {"pagination": {"next_page": None}}},
    ("GET", "/hetzner/v1/ssh_keys"): {"ssh_keys": [{"id": 9, "fingerprint": "aa"}], "meta": {"pagination": {"next_page": None}}},
    ("GET", "/hetzner/v1/firewalls"): {"firewalls": [{"id": 3}], "meta": {"pagination": {"next_page": None}}},
    ("GET", "/hetzner/v1/networks"): {"networks": [{"id": 4}], "meta": {"pagination": {"next_page": None}}},
    ("POST", "/hetzner/v1/ssh_keys"): {"ssh_key": {"id": 10}},
    ("POST", "/hetzner/v1/servers"): {"server": {"id": 777, "public_net": {"ipv4": {"ip": "198.51.100.91"}, "ipv6": {"ip": "2001:db8::/64"}}}},
    ("POST", "/hetzner/v1/servers/777/actions/enable_backup"): {"action": {"id": 1}},
    ("GET", "/vultr/v2/regions"): {"regions": [{"id": "ams"}], "meta": {"links": {"next": ""}}},
    ("GET", "/vultr/v2/plans"): {"plans": [{"id": "vc2"}], "meta": {"links": {"next": ""}}},
    ("GET", "/vultr/v2/os"): {"os": [{"id": 1}], "meta": {"links": {"next": ""}}},
    ("GET", "/vultr/v2/ssh-keys"): {"ssh_keys": [{"id": "k1", "ssh_key": "ssh-rsa AAA x"}], "meta": {"links": {"next": ""}}},
    ("POST", "/vultr/v2/ssh-keys"): {"ssh_key": {"id": "k2"}},
    ("POST", "/vultr/v2/instances"): {"instance": {"id": "vi-1", "status": "pending", "main_ip": "198.51.100.92", "v6_main_ip": ""}},
    ("GET", "/digitalocean/v2/regions"): {"regions": [{"slug": "a", "available": True}, {"slug": "b", "available": False}], "links": {}},
    ("GET", "/digitalocean/v2/sizes"): {"sizes": [{"slug": "s"}], "links": {}},
    ("GET", "/digitalocean/v2/images"): {"images": [{"id": 1, "public": True}, {"id": 2, "public": False}], "links": {}},
    ("GET", "/digitalocean/v2/account/keys"): {"ssh_keys": [{"id": 11, "fingerprint": "bb"}], "links": {}},
    ("POST", "/digitalocean/v2/account/keys"): {"ssh_key": {"id": 12}},
    ("POST", "/digitalocean/v2/droplets"): {"droplet": {"id": 888, "status": "new", "networks": {"v4": [{"type": "public", "ip_address": "198.51.100.93"}], "v6": []}}},
    ("GET", "/hostinger/api/vps/v1/data-centers"): [{"id": 1}],
    ("GET", "/hostinger/api/vps/v1/templates"): [{"id": 2}],
    ("GET", "/hostinger/api/billing/v1/catalog"): [{"id": "hostingercom-vps-kvm2"}, {"id": "other"}],
    ("GET", "/hostinger/api/vps/v1/public-keys"): {"data": [{"id": 3}]},
    ("GET", "/hostinger/api/vps/v1/post-install-scripts"): {"data": [{"id": 4}]},
    ("GET", "/hostinger/api/vps/v1/virtual-machines"): [],
    ("POST", "/hostinger/api/vps/v1/virtual-machines"): {"virtual_machine": {"id": 999, "state": "running", "ipv4": [{"address": "198.51.100.94"}], "ipv6": []}},
}
HOSTS = {"https://api.hetzner.cloud/v1": "/hetzner/v1", "https://api.vultr.com/v2": "/vultr/v2",
         "https://api.digitalocean.com/v2": "/digitalocean/v2", "https://developers.hostinger.com": "/hostinger"}
RS_REQUESTS = []


class Fake(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _serve(self):
        n = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(n).decode() if n else ""
        path, _, query = self.path.partition("?")
        RS_REQUESTS.append({"method": self.command, "path": path, "query": query,
                            "auth": self.headers.get("Authorization"), "accept": self.headers.get("Accept"),
                            "body": json.loads(body) if body else None})
        data = FAKE.get((self.command, path))
        status = 200 if data is not None else 404
        out = json.dumps(data if data is not None else {"error": {"message": "unknown"}}).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)

    do_GET = do_POST = do_DELETE = do_PATCH = _serve


PHP = r"""
$fake = json_decode(file_get_contents('/tmp/servers_diff_fake.json'), true);
$hosts = ['https://api.hetzner.cloud/v1'=>'/hetzner/v1','https://api.vultr.com/v2'=>'/vultr/v2','https://api.digitalocean.com/v2'=>'/digitalocean/v2','https://developers.hostinger.com'=>'/hostinger'];
$log = [];
\Illuminate\Support\Facades\Http::fake(function ($request) use (&$log, $fake, $hosts) {
    $url = $request->url(); $path = parse_url($url, PHP_URL_PATH); $host = parse_url($url, PHP_URL_SCHEME).'://'.parse_url($url, PHP_URL_HOST);
    $prefix = ''; foreach ($hosts as $h => $p) { if (str_starts_with($url, $h)) { $prefix = $p; $path = substr(explode('?', $url)[0], strlen($h)); } }
    $body = $request->body();
    $log[] = ['method'=>$request->method(),'path'=>$prefix.$path,'query'=>(string) parse_url($url, PHP_URL_QUERY),'auth'=>$request->header('Authorization')[0] ?? null,'accept'=>$request->header('Accept')[0] ?? null,'body'=>$body === '' ? null : json_decode($body, true)];
    $key = $request->method().' '.$prefix.$path;
    return isset($fake[$key]) ? \Illuminate\Support\Facades\Http::response($fake[$key], 200) : \Illuminate\Support\Facades\Http::response(['error'=>['message'=>'unknown']], 404);
});
$calls = json_decode(file_get_contents('/tmp/servers_diff_calls.json'), true);
$out = [];
foreach ($calls as $c) {
    $log = [];
    $server = ['HTTP_ACCEPT'=>'application/json','CONTENT_TYPE'=>'application/json','HTTP_AUTHORIZATION'=>'Bearer '.$c['token']];
    $req = \Illuminate\Http\Request::create($c['path'], $c['method'], [], [], [], $server, $c['body'] === null ? null : json_encode($c['body']));
    app()->instance('request', $req);
    \Illuminate\Support\Facades\Auth::forgetGuards();
    $res = app(\Illuminate\Contracts\Http\Kernel::class)->handle($req);
    $out[] = ['status'=>$res->getStatusCode(),'body'=>$res->getContent(),'requests'=>$log];
}
echo "@@@".json_encode($out)."@@@";
"""


def cloud():
    server = ThreadingHTTPServer(("127.0.0.1", FAKE_PORT), Fake)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    fake_php = {f"{m} {p}": v for (m, p), v in FAKE.items()}
    enc = 'php /var/www/html/artisan tinker --execute'
    tokens = {}
    made = []
    try:
        # One token per provider, written by the reference so that the cast is upstream's.
        for provider in ("hetzner", "vultr", "digitalocean", "hostinger"):
            script = (f"$t=\\App\\Models\\CloudProviderToken::create(['team_id'=>4,'provider'=>'{provider}','token'=>'tok-{provider}','name'=>'diff-{provider}']);"
                      "echo '@@@'.$t->uuid.'@@@';")
            r = subprocess.run(["docker", "exec", "coolify-rs-reference-coolify-1", "php", "/var/www/html/artisan", "tinker", "--execute", script],
                               capture_output=True, text=True)
            tokens[provider] = re.search(r"@@@(.*?)@@@", r.stdout).group(1)
            made.append(tokens[provider])
        calls = []
        def add(name, method, path, body=None, tok="acme_owner_root"):
            calls.append({"name": name, "method": method, "path": path, "body": body, "tok": tok})
        for provider, routes in (("hetzner", ["locations", "server-types", "images", "ssh-keys", "firewalls", "networks"]),
                                 ("vultr", ["regions", "plans", "os", "ssh-keys"]),
                                 ("digitalocean", ["regions", "sizes", "images", "ssh-keys"]),
                                 ("hostinger", ["data-centers", "catalog", "templates", "ssh-keys", "post-install-scripts"])):
            for route in routes:
                add(f"{provider}/{route}", "GET", f"/api/v1/{provider}/{route}?cloud_provider_token_uuid={tokens[provider]}")
                add(f"{provider}/{route} member", "GET", f"/api/v1/{provider}/{route}?cloud_provider_token_uuid={tokens[provider]}", tok="acme_member_read")
        create = [
            ("hetzner", {"cloud_provider_token_uuid": tokens["hetzner"], "location": "fsn1", "server_type": "cx11", "image": 5,
                         "name": "Diff-Hetzner", "private_key_uuid": KEY_UUID, "enable_backups": True, "hetzner_ssh_key_ids": [9, 9],
                         "hetzner_firewall_ids": [3], "hetzner_network_ids": [4], "cloud_init_script": "#!/bin/sh\necho x"}),
            ("vultr", {"cloud_provider_token_uuid": tokens["vultr"], "region": "ams", "plan": "vc2", "os_id": 1, "name": "Diff-Vultr",
                       "private_key_uuid": KEY_UUID, "vultr_ssh_key_ids": ["k1"], "cloud_init_script": "#!/bin/sh\necho x"}),
            ("digitalocean", {"cloud_provider_token_uuid": tokens["digitalocean"], "region": "a", "size": "s", "image": 1, "name": "Diff-Do",
                              "private_key_uuid": KEY_UUID, "digitalocean_ssh_key_ids": [11], "monitoring": False}),
            ("hostinger", {"cloud_provider_token_uuid": tokens["hostinger"], "item_id": "hostingercom-vps-kvm2", "data_center_id": 1, "template_id": 2,
                           "name": "diff-hostinger.example.com", "private_key_uuid": KEY_UUID, "enable_backups": True, "post_install_script_id": 4}),
        ]
        for provider, body in create:
            add(f"servers/{provider} without deploy", "POST", f"/api/v1/servers/{provider}", body, tok="acme_owner_write")
            add(f"servers/{provider}", "POST", f"/api/v1/servers/{provider}", body)
            add(f"cloud token {provider} used", "DELETE", f"/api/v1/cloud-tokens/{tokens[provider]}")
        add("token show", "GET", f"/api/v1/cloud-tokens/{tokens['hetzner']}")
        add("token show read", "GET", f"/api/v1/cloud-tokens/{tokens['hetzner']}", tok="acme_owner_read")
        add("token show member", "GET", f"/api/v1/cloud-tokens/{tokens['hetzner']}", tok="acme_member_read")
        add("tokens", "GET", "/api/v1/cloud-tokens")
        add("token rename", "PATCH", f"/api/v1/cloud-tokens/{tokens['hetzner']}", {"name": "diff-hetzner"})
        add("token validate", "POST", f"/api/v1/cloud-tokens/{tokens['hetzner']}/validate")
        # Reference: everything in one process with Http::fake.
        payload = [{"method": c["method"], "path": c["path"], "body": c["body"], "token": TOKENS[c["tok"]]} for c in calls]
        for name, data in (("/tmp/servers_diff_fake.json", fake_php), ("/tmp/servers_diff_calls.json", payload)):
            subprocess.run(["docker", "exec", "-i", "coolify-rs-reference-coolify-1", "sh", "-c", f"cat > {name}"],
                           input=json.dumps(data), text=True, check=True)
        r = subprocess.run(["docker", "exec", "coolify-rs-reference-coolify-1", "php", "/var/www/html/artisan", "tinker", "--execute", PHP],
                           capture_output=True, text=True)
        m = re.search(r"@@@(.*)@@@", r.stdout, re.S)
        if not m:
            raise SystemExit("reference failed: " + r.stdout[-2000:] + r.stderr[-2000:])
        ref = json.loads(m.group(1))
        # Servers the reference made are removed, so that coolify-rs can use the same addresses.
        for ip in ("198.51.100.91", "198.51.100.92", "198.51.100.93", "198.51.100.94"):
            for u in sql(f"select uuid from servers where ip='{ip}'").split("\n"):
                if u:
                    delete_server(u)
        vars_ = {}
        for i, c in enumerate(calls):
            RS_REQUESTS.clear()
            st, body = call("rs", c["method"], c["path"], c["body"], tok=c["tok"])
            ra, rb = ref[i], {"status": st, "body": body, "requests": list(RS_REQUESTS)}
            def n(t):
                t = re.sub(r'"uuid":"[a-z0-9]+"', '"uuid":"U"', t)
                t = re.sub(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+Z", "<ts>", t)
                return t
            ok = ra["status"] == rb["status"] and n(ra["body"]) == n(rb["body"])
            record(f"{c['name']} response", ok, f"ref {ra['status']} {ra['body'][:600]}\n  rs  {rb['status']} {rb['body'][:600]}")
            def reqs(rs):
                # reqwest adds `Accept: */*` where Guzzle sends none; the meaning is the same.
                return [{k: (None if k == "accept" and x.get(k) == "*/*" else x.get(k))
                         for k in ("method", "path", "query", "auth", "accept", "body")} for x in rs]
            ok = reqs(ra["requests"]) == reqs(rb["requests"])
            record(f"{c['name']} provider requests", ok, f"ref {json.dumps(reqs(ra['requests']))[:900]}\n  rs  {json.dumps(reqs(rb['requests']))[:900]}")
        # Rows of the servers coolify-rs made.
        for ip, col in (("198.51.100.91", "hetzner_server_id"), ("198.51.100.92", "vultr_instance_id"),
                        ("198.51.100.93", "digitalocean_droplet_id"), ("198.51.100.94", "hostinger_virtual_machine_id")):
            v = sql(f"select {col} from servers where ip='{ip}'")
            record(f"server row {col}", bool(v), "missing")
    finally:
        for ip in ("198.51.100.91", "198.51.100.92", "198.51.100.93", "198.51.100.94"):
            for u in sql(f"select uuid from servers where ip='{ip}'").split("\n"):
                if u:
                    delete_server(u)
        for u in made:
            sql(f"delete from cloud_provider_tokens where uuid='{u}'")
        server.shutdown()


# ── Jobs on real throwaway VMs ──────────────────────────────────────────


def vm(machine, *cmd, check=False):
    r = subprocess.run(["orb", "-m", machine, "-u", "root", *cmd], capture_output=True, text=True)
    if check and r.returncode:
        raise SystemExit(f"{machine} {cmd}: {r.stderr}")
    return r.stdout


def vm_file(machine, path):
    return vm(machine, "cat", path)


def wait_for(name, fn, timeout=420, step=5):
    end = time.time() + timeout
    while time.time() < end:
        v = fn()
        if v:
            return v
        time.sleep(step)
    print(f"timed out waiting for {name}")
    return None


def jobs(vm_a, vm_b):
    a = create_server("ref", "diff-job-a", f"{vm_a}.orb.local", proxy_type="traefik")
    b = create_server("ref", "diff-job-b", f"{vm_b}.orb.local", proxy_type="traefik")
    S.set("s", a, b)
    sids = {"ref": sql(f"select id from servers where uuid='{a}'"), "rs": sql(f"select id from servers where uuid='{b}'")}
    S.vars["ref"]["sid"], S.vars["rs"]["sid"] = sids["ref"], sids["rs"]
    S.vars["ref"]["sname"], S.vars["rs"]["sname"] = "diff-job-a", "diff-job-b"
    S.vars["ref"]["sip"], S.vars["rs"]["sip"] = f"{vm_a}.orb.local", f"{vm_b}.orb.local"
    machines = {"ref": vm_a, "rs": vm_b}

    def settings(side):
        return row("server_settings", f"server_id={sids[side]}")

    def proxy(side):
        return json.loads(sql(f"select proxy from servers where id={sids[side]}") or "{}")

    def both_file(name, path):
        x, y = vm_file(vm_a, path), vm_file(vm_b, path)
        def n(t):
            return re.sub(r"\.orb\.local", "", t).replace(vm_a, "VM").replace(vm_b, "VM").replace("diff-job-a", "S").replace("diff-job-b", "S")
        record(f"file {name}", bool(x) and n(x) == n(y), f"ref {x[:800]}\n  rs  {y[:800]}")

    def ps(machine):
        out = vm(machine, "docker", "ps", "-a", "--format", "{{.Names}} {{.Image}} {{.Labels}}")
        return sorted(l.split(" ")[0] + " " + l.split(" ")[1] for l in out.splitlines() if l.strip())

    try:
        # Install prerequisites, Docker, the proxy and Sentinel on both.
        same("validate install", "POST", "/api/v1/servers/{s}/validate", {"install": True})
        for side in ("ref", "rs"):
            done = wait_for(f"{side} install", lambda: sql(f"select is_validating::text from servers where id={sids[side]}") == "false"
                            and settings(side)["docker_version"] and proxy(side).get("status") == "running", timeout=600)
            record(f"{side} installed", bool(done), json.dumps(settings(side))[:300] + str(proxy(side).get("status")))
        time.sleep(20)
        for f in ("/etc/docker/daemon.json", "/data/coolify/proxy/docker-compose.yml", "/data/coolify/proxy/dynamic/default_redirect_503.yaml",
                  "/data/coolify/proxy/error-pages/503.html", "/data/coolify/proxy/dynamic/Caddyfile"):
            both_file(f, f)
        record("containers", ps(vm_a) == ps(vm_b), f"{ps(vm_a)}\n  {ps(vm_b)}")
        rows_same("settings after install", "server_settings", f"server_id={sids['ref']}", f"server_id={sids['rs']}",
                  ignore=("sentinel_token", "docker_version_checked_at", "compose_version_checked_at", "sentinel_custom_url"))
        pa, pb = proxy("ref"), proxy("rs")
        record("proxy attribute keys", list(sorted(pa)) == list(sorted(pb)), f"{sorted(pa)}\n  {sorted(pb)}")
        record("proxy configuration", pa.get("last_saved_proxy_configuration") == pb.get("last_saved_proxy_configuration"),
               f"{pa.get('last_saved_proxy_configuration')}\n---\n{pb.get('last_saved_proxy_configuration')}")
        ca = {s: sql(f"select count(*) from ssl_certificates where server_id={sids[s]} and is_ca_certificate") for s in sids}
        record("CA certificate row", ca["ref"] == ca["rs"] == "1", str(ca))
        for m in (vm_a, vm_b):
            subj = vm(m, "openssl", "x509", "-in", "/data/coolify/ssl/coolify-ca.crt", "-noout", "-subject", "-ext", "basicConstraints,keyUsage")
            record(f"CA certificate on {m}", "Coolify CA Certificate" in subj and "CA:TRUE" in subj and "Certificate Sign" in subj, subj)
        rows_same("server after install", "servers", f"id={sids['ref']}", f"id={sids['rs']}",
                  ignore=("ip", "ip_previous", "sentinel_updated_at", "sentinel_waiting_since", "proxy", "server_metadata",
                          "detected_traefik_version", "traefik_outdated_info"))
        meta = {s: json.loads(sql(f"select server_metadata from servers where id={sids[s]}") or "{}") for s in sids}
        record("metadata keys", sorted(meta["ref"]) == sorted(meta["rs"]), str(meta))
        tv = {s: sql(f"select detected_traefik_version from servers where id={sids[s]}") for s in sids}
        record("traefik version", tv["ref"] == tv["rs"] and bool(tv["ref"]), str(tv))
        ti = {s: sql(f"select traefik_outdated_info->>'type' from servers where id={sids[s]}") for s in sids}
        record("traefik outdated info", ti["ref"] == ti["rs"], str(ti))
        # Sentinel.
        for side in ("ref", "rs"):
            wait_for(f"{side} sentinel", lambda: "coolify-sentinel" in " ".join(ps(machines[side])), timeout=240)
        def senv(machine):
            out = vm(machine, "docker", "inspect", "--format", "{{json .Config.Env}}", "coolify-sentinel")
            return sorted(e for e in json.loads(out or "[]") if not e.startswith(("TOKEN=", "PATH=", "PUSH_ENDPOINT=")))
        record("sentinel env", senv(vm_a) == senv(vm_b), f"{senv(vm_a)}\n  {senv(vm_b)}")
        same("restart sentinel by setting", "PATCH", "/api/v1/servers/{s}/sentinel", {"sentinel_push_interval_seconds": 45})
        for side in ("ref", "rs"):
            changed = wait_for(f"{side} sentinel env", lambda: "PUSH_INTERVAL_SECONDS=45" in senv(machines[side]), timeout=240)
            record(f"{side} sentinel restarted with the new setting", bool(changed), str(senv(machines[side])))
        record("sentinel env after change", senv(vm_a) == senv(vm_b), f"{senv(vm_a)}\n  {senv(vm_b)}")
        # A push with the server's token on each side.
        tokens = {side: json.loads(call("ref", "GET", f"/api/v1/servers/{S.vars[side]['s']}/sentinel")[1])["sentinel_token"] for side in sids}
        out = {}
        for side in sids:
            r = urllib.request.Request(BASES[side] + "/api/v1/sentinel/push", data=json.dumps({"containers": []}).encode(), method="POST")
            r.add_header("Content-Type", "application/json")
            r.add_header("Authorization", "Bearer " + tokens[side])
            r.add_header("Accept", "application/json")
            try:
                resp = urllib.request.urlopen(r)
                out[side] = (resp.status, resp.read().decode())
            except urllib.error.HTTPError as e:
                out[side] = (e.code, e.read().decode())
        record("sentinel push", out["ref"] == out["rs"], str(out))
        # Destinations.
        st = same("POST destination", "POST", "/api/v1/servers/{s}/destinations", {"network": "diff-net"})
        for side in sids:
            n = vm(machines[side], "docker", "network", "ls", "--format", "{{.Name}}")
            record(f"{side} network created", "diff-net" in n, n)
        same("POST destination existing network", "POST", "/api/v1/servers/{s}/destinations", {"network": "bridge2"})
        uu = {side: sql(f"select uuid from standalone_dockers where server_id={sids[side]} and network='diff-net'") for side in sids}
        S.set("d", uu["ref"], uu["rs"])
        same("DELETE destination", "DELETE", "/api/v1/destinations/{d}")
        for side in sids:
            n = vm(machines[side], "docker", "network", "ls", "--format", "{{.Name}}")
            record(f"{side} network removed", "diff-net" not in n, n)
        # Registries.
        same("GET registries", "GET", "/api/v1/servers/{s}/registries")
        same("POST registry login fails", "POST", "/api/v1/servers/{s}/registries",
             {"registry": "127.0.0.1:1", "username": "u", "password": "secret-pw"})
        same("POST registry check fails", "POST", "/api/v1/servers/{s}/registries/127.0.0.1:1/check")
        same("DELETE registry logout", "DELETE", "/api/v1/servers/{s}/registries/127.0.0.1:1")
        # Proxy redirect and configuration.
        same("PATCH proxy redirect", "PATCH", "/api/v1/servers/{s}/proxy",
             {"redirect_url": "https://1.1.1.1/x", "traefik_certificate_resolver": "myres"})
        both_file("redirect file", "/data/coolify/proxy/dynamic/default_redirect_503.yaml")
        record("error page container removed", ps(vm_a) == ps(vm_b), f"{ps(vm_a)}\n  {ps(vm_b)}")
        same("PATCH proxy error page", "PATCH", "/api/v1/servers/{s}/proxy", {"redirect_url": None, "proxy_error_page": "<h1>Mine</h1>"})
        both_file("redirect file after page", "/data/coolify/proxy/dynamic/default_redirect_503.yaml")
        both_file("custom error page", "/data/coolify/proxy/error-pages/503.html")
        same("PATCH proxy redirect off", "PATCH", "/api/v1/servers/{s}/proxy", {"redirect_enabled": False})
        record("redirect file removed", not vm_file(vm_a, "/data/coolify/proxy/dynamic/default_redirect_503.yaml") and not vm_file(vm_b, "/data/coolify/proxy/dynamic/default_redirect_503.yaml"))
        same("PATCH proxy redirect on", "PATCH", "/api/v1/servers/{s}/proxy", {"redirect_enabled": True, "proxy_error_page": None, "traefik_certificate_resolver": None})
        both_file("redirect file default", "/data/coolify/proxy/dynamic/default_redirect_503.yaml")
        config = proxy("ref")["last_saved_proxy_configuration"]
        same("PUT proxy config bad yaml", "PUT", "/api/v1/servers/{s}/proxy/configuration", {"configuration": "a: ["})
        same("PUT proxy config bad port", "PUT", "/api/v1/servers/{s}/proxy/configuration", {"configuration": "services:\n  traefik:\n    ports:\n      - 'abc'\n"})
        same("PUT proxy config bad name", "PUT", "/api/v1/servers/{s}/proxy/configuration", {"configuration": "services:\n  traefik:\n    container_name: x\n"})
        edited = config.replace("'--api.insecure=false'", "'--api.insecure=false'\n      - '--log.level=INFO'")
        same("PUT proxy config", "PUT", "/api/v1/servers/{s}/proxy/configuration", {"configuration": edited})
        both_file("compose after save", "/data/coolify/proxy/docker-compose.yml")
        for m in (vm_a, vm_b):
            b_ = vm(m, "ls", "/data/coolify/proxy/backups")
            record(f"backup on {m}", "docker-compose." in b_, b_)
        # Restart.
        same("POST proxy restart", "POST", "/api/v1/servers/{s}/proxy/restart")
        time.sleep(15)
        for side in sids:
            wait_for(f"{side} proxy running", lambda: proxy(side).get("status") == "running", timeout=300)
        pa, pb = proxy("ref"), proxy("rs")
        record("proxy after restart", pa.get("status") == pb.get("status") == "running" and pa.get("last_applied_settings") == pb.get("last_applied_settings"),
               f"{pa.get('status')} {pb.get('status')}")
        # Docker cleanup.
        for m in (vm_a, vm_b):
            vm(m, "docker", "pull", "-q", "busybox:1.36")
            vm(m, "docker", "volume", "create", "diff-vol")
            vm(m, "docker", "network", "create", "diff-unused")
        same("POST cleanup", "POST", "/api/v1/servers/{s}/docker-cleanup/run", {"delete_unused_volumes": True, "delete_unused_networks": True})
        for side in sids:
            wait_for(f"{side} cleanup", lambda: sql(f"select status from docker_cleanup_executions where server_id={sids[side]} order by id desc limit 1") == "success"
                     and sql(f"select finished_at is not null from docker_cleanup_executions where server_id={sids[side]} order by id desc limit 1") == "t", timeout=300, step=3)
        log = {side: json.loads(sql(f"select cleanup_log from docker_cleanup_executions where server_id={sids[side]} order by id desc limit 1") or "[]") for side in sids}
        ca_, cb_ = [e["command"] for e in log["ref"]], [e["command"] for e in log["rs"]]
        record("cleanup commands", ca_ == cb_, f"{ca_}\n  {cb_}")
        msg = {side: re.sub(r"\d+%", "N%", sql(f"select message from docker_cleanup_executions where server_id={sids[side]} order by id desc limit 1")) for side in sids}
        record("cleanup message", msg["ref"] == msg["rs"], str(msg))
        for m in (vm_a, vm_b):
            v = vm(m, "docker", "volume", "ls", "--format", "{{.Name}}")
            n = vm(m, "docker", "network", "ls", "--format", "{{.Name}}")
            record(f"cleanup removed on {m}", "diff-vol" not in v and "diff-unused" not in n, v + n)
        # Log drain.
        same("PATCH drain custom", "PATCH", "/api/v1/servers/{s}/log-drains",
             {"is_logdrain_axiom_enabled": True, "logdrain_axiom_dataset_name": "d1", "logdrain_axiom_api_key": "k1"})
        time.sleep(25)
        both_file("log drain compose", "/data/coolify/log-drains/docker-compose.yml")
        both_file("log drain env", "/data/coolify/log-drains/.env")
        both_file("log drain config", "/data/coolify/log-drains/fluent-bit.conf")
        same("PATCH drain off", "PATCH", "/api/v1/servers/{s}/log-drains", {"is_logdrain_axiom_enabled": False})
        time.sleep(10)
        record("log drain removed", ps(vm_a) == ps(vm_b), f"{ps(vm_a)}\n  {ps(vm_b)}")
        # Scheduled jobs, run once on each side.
        def rs_job(job, uuid):
            env = dict(os.environ)
            for k, c, n in (("APP_KEY", "coolify-rs-reference-coolify-1", "APP_KEY"),):
                env[k] = subprocess.run(["docker", "exec", c, "printenv", n], capture_output=True, text=True).stdout.strip()
            pw = subprocess.run(["docker", "exec", "coolify-rs-reference-postgres-1", "printenv", "POSTGRES_PASSWORD"], capture_output=True, text=True).stdout.strip()
            env["DATABASE_URL"] = f"postgres://coolify:{pw}@127.0.0.1:15432/coolify"
            port = BASES["rs"].rsplit(":", 1)[1]
            env["COOLIFY_RS_DATA_DIR"] = os.path.expanduser(f"~/.cache/coolify-rs-dev/{port}/data")
            binary = os.path.expanduser(f"~/.cache/coolify-rs-dev/{port}/coolify-rs")
            if subprocess.run(["nc", "-G", "2", "-z", f"{vm_b}.orb.local", "22"], capture_output=True).returncode:
                env["COOLIFY_RS_SSH_BINARY"] = os.path.join(ROOT, "scripts/ssh-via-orb.sh")
            r = subprocess.run([binary, "server-job", job, uuid], capture_output=True, text=True, env=env)
            return r.returncode, r.stdout.strip().split("\n")[-1] if r.stdout.strip() else "", r.stderr[-400:]

        def ref_job(php):
            r = subprocess.run(["docker", "exec", "coolify-rs-reference-coolify-1", "php", "/var/www/html/artisan", "tinker", "--execute",
                                f"$s=\\App\\Models\\Server::where('uuid','{a}')->first(); {php}"], capture_output=True, text=True)
            return r.stdout.strip()[-400:]

        # Patch check.
        print("ref patch", ref_job("(new \\App\\Jobs\\ServerPatchCheckJob($s, true))->handle();")[:200])
        print("rs patch", rs_job("patch-check", b)[0::2])
        pc = {side: json.loads(sql(f"select patch_check_result from server_settings where server_id={sids[side]}") or "{}") for side in sids}
        record("patch result keys", list(pc["ref"]) == list(pc["rs"]), f"{list(pc['ref'])}\n  {list(pc['rs'])}")
        for k in ("total_updates", "security_updates", "reboot_required", "osId", "package_manager"):
            record(f"patch {k}", pc["ref"].get(k) == pc["rs"].get(k), f"{pc['ref'].get(k)} {pc['rs'].get(k)}")
        names = {side: sorted(u["package"] for u in pc[side].get("updates", [])) for side in sids}
        record("patch packages", names["ref"] == names["rs"], f"{names['ref'][:10]}\n  {names['rs'][:10]}")
        record("patch update keys", (pc["ref"].get("updates") or [{}])[0].keys() == (pc["rs"].get("updates") or [{}])[0].keys(), "")
        err = {side: sql(f"select patch_check_error is null from server_settings where server_id={sids[side]}") for side in sids}
        record("patch error", err["ref"] == err["rs"] == "t", str(err))
        # Storage check: the command is the same; both print the usage.
        rc, out, e = rs_job("storage-check", b)
        real = vm(vm_b, "sh", "-c", "df / --output=pcent | tr -cd 0-9")
        record("storage check", rc == 0 and out == real, f"{rc} {out} {real} {e}")
        # Connection check after losing sshd: two failures mark the server unreachable, then it recovers.
        for side, m in machines.items():
            vm(m, "systemctl", "stop", "ssh", "ssh.socket")
        for _ in range(2):
            ref_job("(new \\App\\Jobs\\ServerConnectionCheckJob($s))->handle();")
            rs_job("connection-check", b)
        down = {side: sql(f"select ss.is_reachable::text||ss.is_usable::text||s.unreachable_count from server_settings ss join servers s on s.id=ss.server_id where s.id={sids[side]}") for side in sids}
        record("connection check down", down["ref"] == down["rs"] == "falsefalse2", str(down))
        for side, m in machines.items():
            vm(m, "systemctl", "start", "ssh")
        time.sleep(5)
        ref_job("(new \\App\\Jobs\\ServerConnectionCheckJob($s))->handle();")
        rs_job("connection-check", b)
        up = {side: sql(f"select ss.is_reachable::text||ss.is_usable::text||s.unreachable_count from server_settings ss join servers s on s.id=ss.server_id where s.id={sids[side]}") for side in sids}
        record("connection check up", up["ref"] == up["rs"] == "truetrue0", str(up))
        # Traefik check with an older version stored as running.
        for m in (vm_a, vm_b):
            pass
        ref_job("(new \\App\\Jobs\\CheckTraefikVersionForServerJob($s, get_traefik_versions()))->handle();")
        rs_job("traefik-check", b)
        tv = {side: sql(f"select coalesce(detected_traefik_version,'')||'|'||coalesce(traefik_outdated_info->>'type','')||'|'||coalesce(traefik_outdated_info->>'latest','') from servers where id={sids[side]}") for side in sids}
        record("traefik check", tv["ref"] == tv["rs"], str(tv))
        # Server check restarts a removed proxy.
        for m in (vm_a, vm_b):
            vm(m, "docker", "rm", "-f", "coolify-proxy")
        ref_job("(new \\App\\Jobs\\ServerCheckJob($s))->handle();")
        rs_job("server-check", b)
        for side, m in machines.items():
            wait_for(f"{side} proxy back", lambda: "coolify-proxy " in " ".join(ps(m)), timeout=180)
        record("server check restarts the proxy", ps(vm_a) == ps(vm_b) and any(x.startswith("coolify-proxy ") for x in ps(vm_b)), f"{ps(vm_a)}\n  {ps(vm_b)}")
        # Proxy type change.
        same("PATCH proxy type caddy", "PATCH", "/api/v1/servers/{s}/proxy", {"proxy_type": "caddy"})
        for side in sids:
            wait_for(f"{side} caddy", lambda: proxy(side).get("status") == "running" and proxy(side).get("type") == "CADDY", timeout=300)
        time.sleep(10)
        both_file("caddy compose", "/data/coolify/proxy/caddy/docker-compose.yml")
        both_file("caddy redirect", "/data/coolify/proxy/caddy/dynamic/default_redirect_503.caddy")
        both_file("caddyfile", "/data/coolify/proxy/caddy/dynamic/Caddyfile")
        record("containers on caddy", ps(vm_a) == ps(vm_b), f"{ps(vm_a)}\n  {ps(vm_b)}")
    finally:
        print("cleanup of servers", a, b)
        delete_server(a)
        delete_server(b)


def main():
    p = argparse.ArgumentParser()
    p.add_argument("what", choices=["api", "cloud", "jobs"])
    p.add_argument("--vm-a")
    p.add_argument("--vm-b")
    args = p.parse_args()
    if args.what == "api":
        api()
    elif args.what == "cloud":
        cloud()
    else:
        jobs(args.vm_a, args.vm_b)
    print(f"{sum(results)}/{len(results)} checks passed")
    sys.exit(0 if all(results) else 1)


if __name__ == "__main__":
    main()
