#!/usr/bin/env python3
"""Differential API test: upstream Coolify (reference) vs coolify-rs.

Both servers must point at the SAME database (the reference instance's
Postgres), seeded with scripts/reference-seed.sh. Every case is sent to both
servers and the responses are compared:

* stateless cases (auth errors, validation, reads): status, content type and
  body bytes must be identical;
* write cases: each server performs the write on its own resource; responses
  are compared after normalising generated identifiers and timestamps, then
  each server reads the resource the *other* one wrote (cross-read), proving
  coolify-rs writes rows upstream reads identically and vice versa.

Usage: compat/diff/differential.py --tokens compat/reference/secrets/tokens.json \
         [--reference http://127.0.0.1:18000] [--subject http://127.0.0.1:18080] \
         [--report compat/diff/last-report.json]
Exit code 0 only if every case matches.
"""
import argparse
import json
import re
import sys
import time
import urllib.error
import urllib.request

UUID_RE = re.compile(r'"[a-z0-9]{24}"')
TS_RE = re.compile(r'"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.000000Z"')


class Client:
    def __init__(self, base, tokens):
        self.base = base.rstrip("/")
        self.tokens = tokens

    def req(self, method, path, tok=None, body=None, raw=None, ctype="application/json", headers=None):
        data = raw if raw is not None else (json.dumps(body).encode() if body is not None else None)
        r = urllib.request.Request(self.base + path, data=data, method=method)
        if tok:
            r.add_header("Authorization", "Bearer " + self.tokens[tok])
        if data is not None and ctype:
            r.add_header("Content-Type", ctype)
        for k, v in (headers or {}).items():
            r.add_header(k, v)
        for _ in range(5):
            try:
                resp = urllib.request.urlopen(r, timeout=30)
                return resp.status, resp.headers.get("Content-Type", ""), resp.read()
            except urllib.error.HTTPError as e:
                # Upstream's `throttle:api` (200/min); coolify-rs has no API rate limit yet.
                if e.code == 429 and e.headers.get("Retry-After"):
                    time.sleep(int(e.headers["Retry-After"]) + 1)
                    continue
                return e.code, e.headers.get("Content-Type", ""), e.read()
        return 429, "", b"rate limited"


def norm(body: bytes, names=()) -> str:
    s = body.decode("utf-8", "replace")
    for n in names:
        s = s.replace(n, "<name>")
    s = UUID_RE.sub('"<uuid>"', s)
    s = TS_RE.sub('"<ts>"', s)
    s = re.sub(r'"id":\d+', '"id":<id>', s)
    s = re.sub(r'"project_id":\d+', '"project_id":<id>', s)
    return s


def ctype_family(ct: str) -> str:
    return ct.split(";")[0].strip().lower()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tokens", required=True)
    ap.add_argument("--reference", default="http://127.0.0.1:18000")
    ap.add_argument("--subject", default="http://127.0.0.1:18080")
    ap.add_argument("--report")
    ap.add_argument("--db-url", help="shared database URL; enables IP allowlist cases "
                    "(coolify-rs must run with COOLIFY_RS_TRUSTED_PROXIES=127.0.0.1)")
    a = ap.parse_args()
    tokens = json.load(open(a.tokens))
    ref, sub = Client(a.reference, tokens), Client(a.subject, tokens)
    acme = tokens["acme_team_id"]
    results = []

    def record(name, ok, ref_r, sub_r, note=""):
        results.append({
            "case": name, "match": ok, "note": note,
            "reference": {"status": ref_r[0], "body": ref_r[2].decode("utf-8", "replace")[:500]},
            "subject": {"status": sub_r[0], "body": sub_r[2].decode("utf-8", "replace")[:500]},
        })
        print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"\n   ref={ref_r[0]} {ref_r[2][:300]!r}\n   sub={sub_r[0]} {sub_r[2][:300]!r}"))

    def same(name, method, path, tok=None, body=None, raw=None, ctype="application/json", names=()):
        r1 = ref.req(method, path, tok, body, raw, ctype)
        r2 = sub.req(method, path, tok, body, raw, ctype)
        exact = not names
        ok = r1[0] == r2[0] and ctype_family(r1[1]) == ctype_family(r2[1]) and (
            r1[2] == r2[2] if exact else norm(r1[2], names) == norm(r2[2], names))
        record(name, ok, r1, r2)
        return r1, r2

    # --- unauthenticated / auth chain -------------------------------------------------
    same("health", "GET", "/api/health")
    same("v1 health", "GET", "/api/v1/health")
    same("no token", "GET", "/api/v1/projects")
    same("garbage token", "GET", "/api/v1/projects", raw=None, tok=None)
    tokens_bad = dict(tokens, bad="999999|nope", bad2="nope")
    ref.tokens = sub.tokens = tokens_bad
    same("unknown token id", "GET", "/api/v1/projects", "bad")
    same("bare unknown token", "GET", "/api/v1/projects", "bad2")
    same("write token on read route", "GET", "/api/v1/version", "acme_owner_write")
    same("member with legacy write token", "GET", "/api/v1/projects", "acme_member_write_legacy")
    same("read token on write route", "POST", "/api/v1/projects", "acme_owner_read", {"name": "zzz"})
    same("version", "GET", "/api/v1/version", "acme_owner_read")
    same("unknown route", "GET", "/api/v1/nope", "acme_owner_read")
    same("unknown route no auth", "DELETE", "/api/v1/nope")
    same("wrong method on known path", "DELETE", "/api/v1/projects", "acme_owner_root")
    same("GET disable", "GET", "/api/v1/disable", "acme_owner_write")
    same("POST disable non-root team", "POST", "/api/v1/disable", "acme_owner_write")

    # --- teams ---------------------------------------------------------------------------
    for tok in ("acme_owner_read", "acme_member_read", "root_read", "carol_root"):
        same(f"teams [{tok}]", "GET", "/api/v1/teams", tok)
        same(f"team [{tok}]", "GET", "/api/v1/team", tok)
        same(f"team members [{tok}]", "GET", "/api/v1/team/members", tok)
        same(f"teams/current [{tok}]", "GET", "/api/v1/teams/current", tok)
        same(f"teams/current/members [{tok}]", "GET", "/api/v1/teams/current/members", tok)
    same("teams/{id} own", "GET", f"/api/v1/teams/{acme}", "acme_owner_read")
    same("teams/{id} other", "GET", "/api/v1/teams/0", "acme_owner_read")
    same("teams/{id}/members own", "GET", f"/api/v1/teams/{acme}/members", "acme_member_read")
    same("teams/{id}/members other", "GET", f"/api/v1/teams/{tokens['carol_team_id']}/members", "acme_owner_read")
    same("PATCH team invalid", "PATCH", "/api/v1/team", "acme_owner_write", {"is_build_server_fallback_enabled": "yes"})
    same("PATCH team extra field", "PATCH", "/api/v1/team", "acme_owner_write", {"is_build_server_fallback_enabled": True, "name": "x"})
    same("PATCH team missing", "PATCH", "/api/v1/team", "acme_owner_write", {"foo": 1})
    # Idempotent write: same final state regardless of which server runs it.
    for v in (False, True):
        r1 = ref.req("PATCH", "/api/v1/team", "acme_owner_write", {"is_build_server_fallback_enabled": v})
        r2 = sub.req("PATCH", "/api/v1/team", "acme_owner_write", {"is_build_server_fallback_enabled": v})
        record(f"PATCH team fallback={v}", r1[0] == r2[0] and norm(r1[2]) == norm(r2[2]), r1, r2)

    # --- projects: validation (stateless) -------------------------------------------------
    w = "acme_owner_write"
    same("create empty json", "POST", "/api/v1/projects", w, {})
    same("create name null", "POST", "/api/v1/projects", w, {"name": ""})
    same("create short name, long desc, extra", "POST", "/api/v1/projects", w, {"name": "ab", "description": "x" * 300, "foo": 1})
    same("create bad chars", "POST", "/api/v1/projects", w, {"name": "bad<name>", "description": "ok"})
    same("create unicode symbol", "POST", "/api/v1/projects", w, {"name": "Proj ✓"})
    same("create name int", "POST", "/api/v1/projects", w, {"name": 123})
    same("create desc bad", "POST", "/api/v1/projects", w, {"name": "Good", "description": "hash#tag"})
    same("create whitespace name", "POST", "/api/v1/projects", w, {"name": "   "})
    same("create trimmed short", "POST", "/api/v1/projects", w, {"name": "  ab  "})
    same("create form encoded", "POST", "/api/v1/projects", w, raw=b"name=x", ctype="application/x-www-form-urlencoded")
    same("create bad json", "POST", "/api/v1/projects", w, raw=b"{bad")
    same("create json array", "POST", "/api/v1/projects", w, raw=b"[]")
    same("create query extra", "POST", "/api/v1/projects?foo=bar", w, {"name": "Valid Name"} if False else {"name": "ab"})
    same("member cannot create (read token)", "POST", "/api/v1/projects", "acme_member_read", {"name": "Nope"})
    same("patch missing project invalid body", "PATCH", "/api/v1/projects/doesnotexist", w, {"name": "x"})
    same("patch missing project valid body", "PATCH", "/api/v1/projects/doesnotexist", w, {"name": "Fine"})
    same("show missing project", "GET", "/api/v1/projects/doesnotexist", "acme_owner_read")
    same("delete missing project", "DELETE", "/api/v1/projects/doesnotexist", w)

    # --- projects: writes + cross-reads ----------------------------------------------------
    def create(client, name, desc):
        r = client.req("POST", "/api/v1/projects", w, {"name": name, "description": desc} if desc is not None else {"name": name})
        return r, (json.loads(r[2]).get("uuid") if r[0] == 201 else None)

    rr, ref_uuid = create(ref, "Diff Ref Proj", "made by php")
    sr, sub_uuid = create(sub, "Diff Rs Proj", "made by rust")
    record("create project", rr[0] == sr[0] and norm(rr[2]) == norm(sr[2]), rr, sr)
    rr2, ref_uuid2 = create(ref, "Diff Ref NoDesc", None)
    sr2, sub_uuid2 = create(sub, "Diff Rs NoDesc", None)
    record("create project without description", rr2[0] == sr2[0] and norm(rr2[2]) == norm(sr2[2]), rr2, sr2)

    names = ("Diff Ref Proj", "Diff Rs Proj", "made by php", "made by rust", "Diff Ref NoDesc", "Diff Rs NoDesc")
    if ref_uuid and sub_uuid:
        for label, a_uuid, b_uuid in (("cross-read rust-created", sub_uuid, ref_uuid), ("cross-read php-created", ref_uuid, sub_uuid)):
            # Both servers read the same resource: must be byte-identical.
            same(f"{label}: show", "GET", f"/api/v1/projects/{a_uuid}", "acme_owner_read")
            same(f"{label}: environments", "GET", f"/api/v1/projects/{a_uuid}/environments", "acme_owner_read")
            same(f"{label}: env details", "GET", f"/api/v1/projects/{a_uuid}/production", "acme_owner_read")
            same(f"{label}: other team 404", "GET", f"/api/v1/projects/{a_uuid}", "carol_root")
        same("cross-read no-description projects", "GET", f"/api/v1/projects/{sub_uuid2}", "acme_owner_read")
        same("list after creates", "GET", "/api/v1/projects", "acme_owner_read")

        # Environment lifecycle, each server on its own project.
        for client_name, client, uuid in (("ref", ref, ref_uuid), ("sub", sub, sub_uuid)):
            pass
        steps = [
            ("POST", "/environments", {"name": "staging"}),
            ("POST", "/environments", {"name": "staging"}),
            ("POST", "/environments", {"name": "x1", "description": "d"}),
            ("PATCH", "/environments/staging", {"name": "production"}),
            ("PATCH", "/environments/staging", {"description": "Stage é/ü"}),
            ("PATCH", "/environments/staging", {"name": "  "}),
            ("GET", "/staging", None),
            ("PATCH", "/environments/nope", {"name": "abc"}),
            ("DELETE", "/environments/staging", None),
            ("DELETE", "/environments/staging", None),
            ("PATCH", "", {"description": None}),
            ("PATCH", "", {"name": "Renamed Proj"}),
            ("PATCH", "", {"name": "Renamed Proj", "description": "same"}),
        ]
        for method, suffix, body in steps:
            tok = "acme_owner_read" if method == "GET" else w
            r1 = ref.req(method, f"/api/v1/projects/{ref_uuid}{suffix}", tok, body)
            r2 = sub.req(method, f"/api/v1/projects/{sub_uuid}{suffix}", tok, body)
            record(f"env step {method} {suffix} {body}", r1[0] == r2[0] and norm(r1[2], names) == norm(r2[2], names), r1, r2)
        same("cross-read after env steps (rust project)", "GET", f"/api/v1/projects/{sub_uuid}", "acme_owner_read")
        same("cross-read after env steps (php project)", "GET", f"/api/v1/projects/{ref_uuid}", "acme_owner_read")

        # Deletion: each server deletes the project the other created.
        for label, client, uuid in (("rust deletes php project", sub, ref_uuid), ("php deletes rust project", ref, sub_uuid),
                                    ("rust deletes php project 2", sub, ref_uuid2), ("php deletes rust project 2", ref, sub_uuid2)):
            r = client.req("DELETE", f"/api/v1/projects/{uuid}", w)
            ok = r[0] == 200 and r[2] == b'{"message":"Project deleted."}'
            record(label, ok, r, r)
            same(f"{label}: gone on both", "GET", f"/api/v1/projects/{uuid}", "acme_owner_read")

    # --- private keys (coolify-rs must run with the reference APP_KEY) ----------------
    import os
    import subprocess
    import tempfile

    def new_key(kind):
        d = tempfile.mkdtemp()
        path = os.path.join(d, "k")
        if kind == "pkcs1":
            subprocess.run(["openssl", "genrsa", "-traditional", "-out", path, "2048"], check=True, capture_output=True)
        else:
            subprocess.run(["ssh-keygen", "-q", "-t", kind, "-N", "", "-f", path], check=True)
        return open(path).read()

    created = {}
    for label, client in (("ref", ref), ("sub", sub)):
        for kind in ("ed25519", "pkcs1"):
            r = client.req("POST", "/api/v1/security/keys", w, {"name": f"diff {label} {kind}", "private_key": new_key(kind)})
            created[(label, kind)] = (r, json.loads(r[2]).get("uuid") if r[0] == 201 else None)
    for kind in ("ed25519", "pkcs1"):
        r1, r2 = created[("ref", kind)][0], created[("sub", kind)][0]
        record(f"keys: create {kind}", r1[0] == r2[0] == 201 and norm(r1[2]) == norm(r2[2]), r1, r2)
    for tok in ("acme_owner_read", "acme_owner_root", "acme_member_read", "carol_root"):
        same(f"keys: list [{tok}]", "GET", "/api/v1/security/keys", tok)
    for (label, kind), (_, uuid) in created.items():
        if uuid:
            same(f"keys: show {label}-created {kind} (sensitive)", "GET", f"/api/v1/security/keys/{uuid}", "acme_owner_root")
            same(f"keys: show {label}-created {kind} other team", "GET", f"/api/v1/security/keys/{uuid}", "carol_root")
    dup = new_key("ed25519")
    r1 = ref.req("POST", "/api/v1/security/keys", w, {"private_key": dup})
    r2 = sub.req("POST", "/api/v1/security/keys", w, {"private_key": dup})
    record("keys: duplicate fingerprint", r1[0] == 201 and r2[0] == 422 and r2[2] == b'{"message":"Private key already exists."}', r1, r2)
    same("keys: invalid", "POST", "/api/v1/security/keys", w, {"private_key": "garbage"})
    same("keys: missing key", "POST", "/api/v1/security/keys", w, {"name": "x"})
    same("keys: name not a string", "POST", "/api/v1/security/keys", w, {"name": 5, "private_key": "x"})
    same("keys: member cannot create", "POST", "/api/v1/security/keys", "acme_member_read", {"private_key": "x"})
    ru, su = created[("ref", "ed25519")][1], created[("sub", "ed25519")][1]
    if ru and su:
        json_accept = {"Accept": "application/json"}
        for body in ({"name": "Renamed key"}, {"private_key": "nope"}, {"foo": 1}, {"description": None}):
            r1 = ref.req("PATCH", f"/api/v1/security/keys/{su}", w, body, headers=json_accept)
            r2 = sub.req("PATCH", f"/api/v1/security/keys/{ru}", w, body, headers=json_accept)
            record(f"keys: update {body}", r1[0] == r2[0] and norm(r1[2]) == norm(r2[2]), r1, r2)
        # Documented divergence: upstream redirects non-JSON clients (KNOWN_LIMITATIONS.md).
        r1 = ref.req("PATCH", f"/api/v1/security/keys/{su}", w, {"private_key": "nope"})
        r2 = sub.req("PATCH", f"/api/v1/security/keys/{ru}", w, {"private_key": "nope"})
        record("keys: invalid update without Accept (expected divergence: 302 vs 422)", r1[0] == 302 and r2[0] == 422, r1, r2)
        same("keys: cross-read after update", "GET", "/api/v1/security/keys", "acme_owner_root")
        # Each server deletes the key the other created.
        for label, client, uuid in (("rust deletes php key", sub, ru), ("php deletes rust key", ref, su)):
            r = client.req("DELETE", f"/api/v1/security/keys/{uuid}", w)
            record(label, r[0] == 200 and r[2] == b'{"message":"Private Key deleted."}', r, r)
            same(f"{label}: gone on both", "GET", f"/api/v1/security/keys/{uuid}", "acme_owner_read")

    # --- servers (read side, create, update, delete; no SSH) --------------------------
    skey_path = new_key("ed25519")
    r = ref.req("POST", "/api/v1/security/keys", w, {"name": "diff servers key", "private_key": skey_path})
    skey = json.loads(r[2]).get("uuid") if r[0] == 201 else None
    for tok in ("root_root", "root_read", "acme_owner_root", "acme_owner_read", "acme_member_read", "carol_root"):
        same(f"servers: list [{tok}]", "GET", "/api/v1/servers", tok)
    same("servers: show missing", "GET", "/api/v1/servers/nope", "acme_owner_read")
    bad_bodies = [
        {"ip": ""},
        {"ip": None, "private_key_uuid": None, "name": None, "port": None, "user": "", "description": None},
        {"ip": "bad host!", "private_key_uuid": "x"},
        {"ip": "1.2.3.4", "private_key_uuid": "x", "port": 70000, "user": "bad user", "name": "a\nb",
         "server_role": "x", "is_build_server": "maybe", "extra": 1},
        {"name": 5, "ip": 5, "private_key_uuid": 5, "user": 5, "server_role": 5, "proxy_type": 5, "description": 5},
        {"ip": "1.2.3.4", "private_key_uuid": "nope"},
        {"ip": "1.2.3.4", "private_key_uuid": skey, "proxy_type": "apache"},
        {"ip": "1.2.3.4", "private_key_uuid": skey, "server_role": "deployment"},
        {"ip": "1.2.3.4", "private_key_uuid": skey, "server_role": "both", "is_build_server": True},
    ]
    for b in bad_bodies:
        same(f"servers: create invalid {b}", "POST", "/api/v1/servers", w, b)
    same("servers: member cannot create", "POST", "/api/v1/servers", "acme_member_read", {"ip": "1.2.3.4"})

    def srv_norm(body, uuid):
        t = body.decode("utf-8", "replace").replace(uuid, "<uuid>")
        t = TS_RE.sub('"<ts>"', t)
        t = re.sub(r'"sentinel_token":"[^"]+"', '"sentinel_token":"<tok>"', t)
        t = re.sub(r'(198\.51\.100\.|2001:db8::|192\.0\.2\.)(1|2)\d', r"\1NN", t)
        t = re.sub(r'"(id|server_id)":\d+', r'"\1":<id>', t)
        return t

    srv_cases = [
        {"name": "diff srv", "description": "d <b>x</b>", "ip": "198.51.100.%d", "private_key_uuid": skey},
        {"name": "diff srv v6", "ip": "[2001:DB8::%d]", "private_key_uuid": skey, "proxy_type": "caddy",
         "server_role": "build", "user": " admin ", "port": "2222"},
    ]
    made = []
    for i, c in enumerate(srv_cases):
        pair = {}
        for label, client, n in (("ref", ref, 10 + i), ("sub", sub, 20 + i)):
            body = json.loads(json.dumps(c).replace("%d", str(n)))
            r = client.req("POST", "/api/v1/servers", w, body)
            pair[label] = (r, json.loads(r[2]).get("uuid") if r[0] == 201 else None)
        (r1, u1), (r2, u2) = pair["ref"], pair["sub"]
        record(f"servers: create {i}", r1[0] == r2[0] == 201, r1, r2)
        if u1 and u2:
            made.append((u1, u2))
            for tok in ("acme_owner_read", "acme_owner_root"):
                same(f"servers: cross-read php-created {i} [{tok}]", "GET", f"/api/v1/servers/{u1}", tok)
                same(f"servers: cross-read rust-created {i} [{tok}]", "GET", f"/api/v1/servers/{u2}", tok)
                ra_ = ref.req("GET", f"/api/v1/servers/{u1}", tok)
                rb_ = ref.req("GET", f"/api/v1/servers/{u2}", tok)
                record(f"servers: rows written alike {i} [{tok}]", srv_norm(ra_[2], u1) == srv_norm(rb_[2], u2), ra_, rb_)
    same("servers: list after creates", "GET", "/api/v1/servers", "acme_owner_root")
    if made:
        u1, u2 = made[0]
        steps = [
            {"name": "patched <i>x</i>", "description": None},
            {"port": "2200", "user": " deploy "},
            {"ip": "[2001:DB8::%d]"},
            {"server_role": "build"},
            {"is_build_server": False},
            {"is_terminal_enabled": None},
            {"concurrent_builds": "4", "server_timezone": "Europe/Berlin", "backup_compression_cpu_percentage": 50},
            {"wildcard_domain": "https://apps.example.com"},
            {"server_disk_usage_check_frequency": "@daily"},
            {"server_disk_usage_check_frequency": "61 * * * *"},
            {"proxy_type": "caddy"},
            {"name": None, "user": None, "port": None},
        ]
        for step in steps:
            out = []
            for client, uuid, n in ((ref, u1, 30), (sub, u2, 31)):
                body = json.loads(json.dumps(step).replace("%d", str(n)))
                r = client.req("PATCH", f"/api/v1/servers/{uuid}", w, body)
                show = ref.req("GET", f"/api/v1/servers/{uuid}", "acme_owner_root")
                out.append((r, srv_norm(r[2], uuid), srv_norm(show[2], uuid).replace("::3", "::3")))
            (ra, na, sa), (rb, nb, sb) = out
            sa = re.sub(r"2001:db8::3\d", "2001:db8::N", sa)
            sb = re.sub(r"2001:db8::3\d", "2001:db8::N", sb)
            record(f"servers: update {step}", ra[0] == rb[0] and na == nb and sa == sb, ra, rb)
        same("servers: update missing", "PATCH", "/api/v1/servers/nope", w, {"name": "x"})
        L = json.loads(ref.req("GET", "/api/v1/servers", "root_read")[2])[0]["uuid"]
        same("servers: local server cannot be deleted", "DELETE", f"/api/v1/servers/{L}", "root_root")
        u3 = made[-1][0]
        for q in ("?delete_from_provider=maybe", "?delete_from_provider=yes", "?force=1"):
            same(f"servers: delete {q}", "DELETE", f"/api/v1/servers/{u3}{q}", w)
        # Validation responses that start no job (compat/diff/validate_diff.py covers the job).
        same("servers: validate missing", "POST", "/api/v1/servers/nope/validate", w)
        same("servers: validate with GET", "GET", f"/api/v1/servers/{u3}/validate", w)
        same("servers: validate install not boolean", "POST", f"/api/v1/servers/{u3}/validate", w, {"install": "maybe"})
        same("servers: validate install without deploy", "POST", f"/api/v1/servers/{u3}/validate", w, {"install": True})
        same("servers: member cannot validate", "POST", f"/api/v1/servers/{u3}/validate", "acme_member_write_legacy")
        same("servers: read token cannot validate", "POST", f"/api/v1/servers/{u3}/validate", "acme_owner_read")
        for label, client, (a_uuid, b_uuid) in (("rust deletes php servers", sub, (made[0][0], made[-1][0])),
                                                ("php deletes rust servers", ref, (made[0][1], made[-1][1]))):
            for uuid in dict.fromkeys((a_uuid, b_uuid)):
                r = client.req("DELETE", f"/api/v1/servers/{uuid}", w)
                record(f"{label} {uuid[:6]}", r[0] == 200 and r[2] == b'{"message":"Server deleted."}', r, r)
                same(f"{label} {uuid[:6]}: gone on both", "GET", f"/api/v1/servers/{uuid}", "acme_owner_read")
    if skey:
        ref.req("DELETE", f"/api/v1/security/keys/{skey}", w)

    # --- IP allowlist through a proxy hop (needs DB access) ---------------------------
    if a.db_url:
        import subprocess

        def set_allowed(v):
            sql = "UPDATE instance_settings SET allowed_ips = %s WHERE id = 0" % ("NULL" if v is None else "'" + v + "'")
            subprocess.run(["psql", a.db_url, "-qc", sql], check=True)

        cases = [
            ("CF peer + CF-Connecting-IP allowed", "198.51.100.7", {"X-Forwarded-For": "104.16.1.1", "CF-Connecting-IP": "198.51.100.7"}),
            ("non-CF peer cannot use CF-Connecting-IP", "198.51.100.7", {"X-Forwarded-For": "203.0.113.5", "CF-Connecting-IP": "198.51.100.7"}),
            ("CF range added after 4.5.0 (104.24/14)", "198.51.100.7", {"X-Forwarded-For": "104.24.0.9", "CF-Connecting-IP": "198.51.100.7"}),
            ("forwarded client in CIDR", "203.0.113.0/24", {"X-Forwarded-For": "203.0.113.5"}),
            ("loopback not in allowlist", "10.0.0.0/8", {}),
        ]
        try:
            for name, allowed, headers in cases:
                set_allowed(allowed)
                r1 = ref.req("GET", "/api/v1/version", "acme_owner_read", headers=headers)
                r2 = sub.req("GET", "/api/v1/version", "acme_owner_read", headers=headers)
                record(f"allowlist: {name}", r1[0] == r2[0] and r1[2] == r2[2], r1, r2)
        finally:
            set_allowed(None)

    passed = sum(r["match"] for r in results)
    print(f"\n{passed}/{len(results)} cases match")
    if a.report:
        with open(a.report, "w") as f:
            json.dump({"passed": passed, "total": len(results), "results": results}, f, indent=2)
    sys.exit(0 if passed == len(results) else 1)


if __name__ == "__main__":
    main()
