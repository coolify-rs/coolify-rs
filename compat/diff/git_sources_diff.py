#!/usr/bin/env python3
"""Differential check of the git source API: `/github-apps` and `/gitlab-apps`.

Needs the reference, a coolify-rs instance (COOLIFY_RS_BASE) and compat/diff/fake_git_provider.py
running on FAKE_BASE (a host address all sides reach, e.g. http://<en0 address>:18990), reachable from the reference container.
Provider answers come from that fake, not from GitHub or GitLab. Temporarily allows the fake in
`webhook_allowed_internal_hosts` and restores the setting at the end. Everything it creates is
deleted. Exit status 1 when anything differs.
"""
import json
import os
import re
import subprocess
import sys
import time
import urllib.parse

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from databases_diff import call as raw_call, record, results, sql  # noqa: E402


def call(side, method, path, body=None, tok="acme_owner_write"):
    st, raw = raw_call(side, method, "/api/v1" + path, body, tok)
    try:
        return st, json.loads(raw)
    except ValueError:
        return st, raw.decode(errors="replace")


FAKE = os.environ["FAKE_BASE"]
FAKE_HOST = urllib.parse.urlsplit(FAKE).hostname
GH = {"html_url": f"{FAKE}/gh", "api_url": f"{FAKE}/github"}
GL = {"html_url": f"{FAKE}/gl", "api_url": f"{FAKE}/gitlab/api/v4"}
VOLATILE = re.compile(r'^(id|uuid|created_at|updated_at|private_key_id)$')
TOKENS = ["acme_owner_write", "acme_owner_read", "acme_member_read", "acme_owner_root", "acme_member_write_legacy", "carol_root"]


def norm(v):
    if isinstance(v, dict):
        return {k: ("<v>" if VOLATILE.match(k) and x is not None else norm(x)) for k, x in v.items()}
    if isinstance(v, list):
        return [norm(x) for x in v]
    return v


def show(v):
    return re.sub(r"http://127\.0\.0\.1:\d+/settings", "http://HOST/settings", json.dumps(v, indent=None))


def same(name, method, path_of, body=None, tok="acme_owner_write", ids=(None, None)):
    """Calls both sides; `path_of` may use {id} with a per-side id."""
    out = []
    for side, i in zip(("ref", "rs"), ids):
        path = path_of.replace("{id}", str(i))
        out.append(call(side, method, path, body, tok))
    (sa, a), (sb, b) = out
    na, nb = norm(a), norm(b)
    ok = sa == sb and show(na) == show(nb)
    record(name, ok, "" if ok else f"\n   ref {sa} {show(na)[:1600]}\n   rs  {sb} {show(nb)[:1600]}")
    return out


def row_of(table, ident, hide=()):
    rows = json.loads(sql(f"SELECT coalesce(json_agg(t), '[]') FROM (SELECT * FROM {table} WHERE id = {int(ident)}) t") or "[]")
    if not rows:
        return None
    r = rows[0]
    for k in ("id", "uuid", "created_at", "updated_at", "private_key_id", *hide):
        r.pop(k, None)
    return r


def compare_rows(name, table, a, b, secrets=()):
    ra, rb = row_of(table, a), row_of(table, b)
    for s in secrets:
        # Ciphertexts differ on each write; presence must match.
        ra[s] = bool(ra.get(s))
        rb[s] = bool(rb.get(s))
    record(name, show(ra) == show(rb), "" if show(ra) == show(rb) else f"\n   ref {show(ra)}\n   rs  {show(rb)}")


def main():
    pem = subprocess.run(["openssl", "genrsa", "2048"], capture_output=True, text=True, check=True).stdout
    st, key = call("ref", "POST", "/security/keys", {"name": "gitsrc-diff-key", "private_key": pem}, "acme_owner_write")
    assert st == 201, key
    key_uuid = key["uuid"]
    saved = sql("SELECT coalesce(webhook_allowed_internal_hosts::text, 'NULL') || '|' || webhook_allow_localhost::text FROM instance_settings WHERE id = 0")
    ids = {"gh": [], "gl": []}
    try:
        # --- SafeExternalUrl: GitHub sources may use private networks, GitLab ones may not.
        for tok in TOKENS:
            same(f"list github-apps as {tok}", "GET", "/github-apps", tok=tok)
            same(f"list gitlab-apps as {tok}", "GET", "/gitlab-apps", tok=tok)
        base = dict(name="gitsrc-gh", app_id=4242, installation_id=99, client_id="cid", client_secret="sec",
                    webhook_secret="whs", private_key_uuid=key_uuid, **GH)
        for label, body, tok in [
            ("empty", {}, "acme_owner_write"),
            ("missing fields", {"name": "x"}, "acme_owner_write"),
            ("unknown field", {**base, "foo": 1}, "acme_owner_write"),
            ("bad organization", {**base, "organization": "a b"}, "acme_owner_write"),
            ("organization with slash", {**base, "organization": "a/b"}, "acme_owner_write"),
            ("bad port", {**base, "custom_port": 70000}, "acme_owner_write"),
            ("bad url", {**base, "html_url": "nope"}, "acme_owner_write"),
            ("metadata url", {**base, "html_url": "http://169.254.169.254/x"}, "acme_owner_write"),
            ("loopback url", {**base, "api_url": "http://127.0.0.1:1/x"}, "acme_owner_write"),
            ("localhost url", {**base, "api_url": "http://localhost:1/x"}, "acme_owner_write"),
            ("bad key", {**base, "private_key_uuid": "nope"}, "acme_owner_write"),
            ("not integer", {**base, "app_id": "abc", "installation_id": 1.5}, "acme_owner_write"),
            ("read token", base, "acme_owner_read"),
            ("member", base, "acme_member_write_legacy"),
            ("system wide as non root", {**base, "is_system_wide": True}, "acme_owner_write"),
        ]:
            same(f"github create: {label}", "POST", "/github-apps", body, tok)
        # Real creates.
        pair = same("github create", "POST", "/github-apps", {**base, "organization": "/acme/"})
        ids["gh"] = [pair[0][1]["id"], pair[1][1]["id"]]
        compare_rows("github row", "github_apps", *ids["gh"], secrets=("client_secret", "webhook_secret"))
        for tok in ("acme_owner_write", "acme_owner_read", "acme_member_read"):
            same(f"list github-apps with rows as {tok}", "GET", "/github-apps", tok=tok)
        # Without api_url the URL is derived from html_url.
        derived = same("github create derives api url", "POST", "/github-apps",
                       {**base, "name": "gitsrc-derived", "api_url": None, "html_url": "https://github.example.test:8443"})
        if derived[0][0] == 422 and derived[1][0] == 422:
            pass
        elif derived[0][0] == 201 and derived[1][0] == 201:
            for side, st_body in zip(("ref", "rs"), derived):
                call(side, "DELETE", f"/github-apps/{st_body[1]['id']}")

        # Provider calls through the fake.
        pid = ids["gh"]
        for tok in ("acme_owner_write", "acme_member_read", "carol_root"):
            same(f"repositories as {tok}", "GET", "/github-apps/{id}/repositories", tok=tok, ids=pid)
            same(f"branches as {tok}", "GET", "/github-apps/{id}/repositories/o/r/branches", tok=tok, ids=pid)
        same("branches missing repo", "GET", "/github-apps/{id}/repositories/o/missing/branches", ids=pid)
        same("repositories unknown app", "GET", "/github-apps/999999/repositories")
        same("repositories id not a number", "GET", "/github-apps/abc/repositories")
        same("update id not a number", "PATCH", "/github-apps/abc", {"name": "x"})
        same("delete id not a number", "DELETE", "/gitlab-apps/abc")
        call("rs", "GET", "/github-apps")

        # Updates.
        for label, body, tok in [
            ("empty body", {}, "acme_owner_write"),
            ("rename", {"name": "gitsrc-renamed"}, "acme_owner_write"),
            ("organization normalized", {"organization": "/org/"}, "acme_owner_write"),
            ("organization cleared", {"organization": None}, "acme_owner_write"),
            ("bad organization", {"organization": "a b"}, "acme_owner_write"),
            ("bad url", {"html_url": "nope"}, "acme_owner_write"),
            ("derive api url", {"html_url": f"{FAKE}/gh"}, "acme_owner_write"),
            ("port", {"custom_port": 2222}, "acme_owner_write"),
            ("bad port", {"custom_port": 70000}, "acme_owner_write"),
            ("secrets", {"client_secret": "new", "webhook_secret": "new2"}, "acme_owner_write"),
            ("bad key", {"private_key_uuid": "nope"}, "acme_owner_write"),
            ("good key", {"private_key_uuid": key_uuid}, "acme_owner_write"),
            ("extra field ignored", {"foo": 1, "name": "gitsrc-renamed-2"}, "acme_owner_write"),
            ("system wide as non root", {"is_system_wide": True}, "acme_owner_write"),
            ("as read", {"name": "x"}, "acme_owner_read"),
            ("as member", {"name": "x"}, "acme_member_write_legacy"),
            ("as other team", {"name": "x"}, "carol_root"),
        ]:
            same(f"github update: {label}", "PATCH", "/github-apps/{id}", body, tok, ids=pid)
            compare_rows(f"github row after: {label}", "github_apps", *pid, secrets=("client_secret", "webhook_secret"))
        same("github update unknown", "PATCH", "/github-apps/999999", {"name": "x"})
        # Restore the fake URL for the next steps.
        for side, i in zip(("ref", "rs"), pid):
            call(side, "PATCH", f"/github-apps/{i}", GH)

        # GitLab: SafeExternalUrl without private networks.
        gl = dict(name="gitsrc-gl", client_id="cid", client_secret="sec", redirect_uri="http://10.0.0.1/cb", webhook_token="tok", **GL)
        for label, body, tok in [
            ("empty", {}, "acme_owner_write"),
            ("missing", {"name": "x"}, "acme_owner_write"),
            ("unknown field", {**gl, "foo": 1}, "acme_owner_write"),
            ("private url refused", gl, "acme_owner_write"),
            ("bad port", {**gl, "custom_port": 0}, "acme_owner_write"),
            ("read token", gl, "acme_owner_read"),
            ("member", gl, "acme_member_write_legacy"),
        ]:
            same(f"gitlab create: {label}", "POST", "/gitlab-apps", body, tok)
        sql(f"UPDATE instance_settings SET webhook_allowed_internal_hosts = '{json.dumps([FAKE_HOST])}' WHERE id = 0")
        time.sleep(1)
        pair = same("gitlab create", "POST", "/gitlab-apps", {**gl, "html_url": f"{FAKE}/gl/"})
        ids["gl"] = [pair[0][1]["id"], pair[1][1]["id"]]
        compare_rows("gitlab row", "gitlab_apps", *ids["gl"], secrets=("client_secret", "webhook_token"))
        pid = ids["gl"]
        pair = same("gitlab create without webhook token", "POST", "/gitlab-apps",
                    {"name": "gitsrc-gl2", "html_url": f"{FAKE}/gl"}, "acme_owner_write")
        for side, st_body in zip(("ref", "rs"), pair):
            if st_body[0] == 201:
                record(f"gitlab generated webhook token on {side}",
                       sql(f"SELECT length(webhook_token) > 40 FROM gitlab_apps WHERE id = {st_body[1]['id']}") == "t")
                call(side, "DELETE", f"/gitlab-apps/{st_body[1]['id']}")
        for tok in ("acme_owner_write", "acme_owner_read", "acme_member_read", "carol_root"):
            same(f"list gitlab-apps with rows as {tok}", "GET", "/gitlab-apps", tok=tok)
        for label, body, tok in [
            ("empty", {}, "acme_owner_write"),
            ("rename", {"name": "gitsrc-gl-renamed"}, "acme_owner_write"),
            ("group", {"group_name": "acme"}, "acme_owner_write"),
            ("group cleared", {"group_name": None}, "acme_owner_write"),
            ("derive api", {"html_url": f"{FAKE}/gl/"}, "acme_owner_write"),
            ("trailing api slash", {"api_url": f"{FAKE}/gitlab/api/v4/"}, "acme_owner_write"),
            ("bad url", {"api_url": "nope"}, "acme_owner_write"),
            ("metadata url", {"api_url": "http://169.254.169.254/"}, "acme_owner_write"),
            ("secret", {"client_secret": "x", "webhook_token": "y"}, "acme_owner_write"),
            ("system wide as non root", {"is_system_wide": True}, "acme_owner_write"),
            ("as read", {"name": "x"}, "acme_owner_read"),
            ("as member", {"name": "x"}, "acme_member_write_legacy"),
            ("as other team", {"name": "x"}, "carol_root"),
        ]:
            same(f"gitlab update: {label}", "PATCH", "/gitlab-apps/{id}", body, tok, ids=pid)
            compare_rows(f"gitlab row after: {label}", "gitlab_apps", *pid, secrets=("client_secret", "webhook_token"))
        same("gitlab update unknown", "PATCH", "/gitlab-apps/999999", {"name": "x"})

        # Deletes, also refused while an application uses the source.
        same("github delete as member", "DELETE", "/github-apps/{id}", tok="acme_member_write_legacy", ids=ids["gh"])
        same("github delete as other team", "DELETE", "/github-apps/{id}", tok="carol_root", ids=ids["gh"])
        same("gitlab delete as member", "DELETE", "/gitlab-apps/{id}", tok="acme_member_write_legacy", ids=ids["gl"])
        same("github delete unknown", "DELETE", "/github-apps/999999")
        same("gitlab delete unknown", "DELETE", "/gitlab-apps/999999")
    finally:
        value, allow = saved.rsplit("|", 1)
        val = "NULL" if value == "NULL" else "'" + value.replace("'", "''") + "'"
        sql(f"UPDATE instance_settings SET webhook_allowed_internal_hosts = {val}, webhook_allow_localhost = {allow} WHERE id = 0")
    if ids["gl"]:
        same("gitlab delete", "DELETE", "/gitlab-apps/{id}", ids=ids["gl"])
    if ids["gh"]:
        # The hook keeps a key other Apps still use, so the shared key stays until the second delete.
        keep = same("github delete keeps a shared key", "DELETE", "/github-apps/{id}", ids=[ids["gh"][0], ids["gh"][1]])
        record("github delete removed the unused key",
               sql(f"SELECT count(*) FROM private_keys WHERE uuid = '{key_uuid}'") == "0", str(keep))
    call("ref", "DELETE", f"/security/keys/{key_uuid}")
    failed = results.count(False)
    print(f"\n{len(results) - failed}/{len(results)} checks passed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
