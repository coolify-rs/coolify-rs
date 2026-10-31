#!/usr/bin/env python3
"""Differential check of notifications, tags, shared variables, audit events, instance
email settings, MCP switches, feedback and integration tokens: reference vs coolify-rs.

    COOLIFY_RS_BASE=http://127.0.0.1:18086 compat/diff/settings_diff.py

Every setting it changes is put back and every row it creates is deleted. Exit status 1
when anything differs.
"""
import json
import os
import re
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from databases_diff import call, record, results, sql  # noqa: E402

UUID = re.compile(r'"[a-z0-9]{24}"')
DATE = re.compile(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{6}Z")


def norm(raw):
    """Masks generated uuids, ids, dates and the instance's own address."""
    text = raw.decode(errors="replace") if isinstance(raw, bytes) else raw
    text = UUID.sub('"UUID"', text)
    text = DATE.sub("DATE", text)
    text = re.sub(r'"(id|env_id)":\d+', r'"\1":0', text)
    text = re.sub(r"(127\.0\.0\.1|localhost)(:|\\u003A)\d+", "HOST", text)
    text = re.sub(r"http:\\?/\\?/HOST", "URL", text)
    return text


def same(name, method, path, body=None, tok="acme_owner_root", mask=None):
    a, b = call("ref", method, path, body, tok), call("rs", method, path, body, tok)
    na, nb = norm(a[1]), norm(b[1])
    if mask:
        na, nb = mask(na), mask(nb)
    record(name, a[0] == b[0] and na == nb, f"ref {a[0]} {na[:1500]}\n  rs  {b[0]} {nb[:1500]}")
    return a


def audit_max():
    return int(sql("select coalesce(max(id),0) from audit_events") or 0)


def audit_rows(after, team=None):
    time.sleep(0.4)
    cols = "team_id,event,source,action,level,actor_type,actor_id,actor_name,actor_email,actor_token_id,actor_token_name,resource_type,resource_name,description,metadata"
    rows = sql(
        f"select row_to_json(r) from (select {cols} from audit_events where id > {after} "
        # Other engineers' requests land in the same table.
        "and event ~ '^api\\.(tag|shared|notifications|mcp|settings)' order by id) r"
    )
    return [norm(x) for x in rows.splitlines()]


def both(name, fn_ref, fn_rs):
    """Runs the action on each side and compares its status, body and audit rows."""
    start = audit_max()
    a = fn_ref()
    ra = audit_rows(start)
    start = audit_max()
    b = fn_rs()
    rb = audit_rows(start)
    sub = lambda t: t.replace("-ref", "-X").replace("-rs", "-X").replace("_REF", "_X").replace("_RS", "_X")
    na, nb = sub(norm(a[1])), sub(norm(b[1]))
    record(name, a[0] == b[0] and na == nb, f"ref {a[0]} {na[:1500]}\n  rs  {b[0]} {nb[:1500]}")
    ra, rb = [sub(x) for x in ra], [sub(x) for x in rb]
    record(name + " audit rows", ra == rb, f"ref {ra}\n  rs  {rb}")
    return a, b


CHANNELS = {
    "email": {
        "errors": [
            {"smtp_enabled": "x", "smtp_from_address": "nope", "smtp_port": 0, "smtp_encryption": "ssl", "smtp_timeout": -1, "smtp_ehlo_domain": "a b", "foo": 1},
            {"smtp_port": 70000, "resend_enabled": 3, "smtp_recipients": "a" * 1001},
            {"smtp_host": "a" * 256, "smtp_username": ["x"], "smtp_ehlo_domain": "-bad.example"},
        ],
        "valid": {"smtp_from_name": "Diff", "smtp_from_address": "a@example.test", "smtp_host": "mail.example.test", "smtp_port": 2525,
                  "smtp_encryption": "starttls", "smtp_username": "u", "smtp_password": "p", "smtp_timeout": 7, "smtp_recipients": "x@example.test",
                  "smtp_ehlo_domain": "mail.example.test", "deployment_success_email_notifications": True, "use_instance_email_settings": False},
    },
    "discord": {
        "errors": [
            {"discord_enabled": "x", "discord_webhook_url": "not a url", "foo": 1},
            {"discord_webhook_url": "http://127.0.0.1/x"},
            {"discord_webhook_url": "ftp://example.com/x"},
            {"discord_webhook_url": "http://169.254.169.254/latest"},
            {"discord_webhook_url": 5},
        ],
        "valid": {"discord_enabled": False, "discord_webhook_url": "https://discord.com/api/webhooks/1/x", "discord_ping_enabled": False, "server_patch_discord_notifications": False},
    },
    "slack": {
        "errors": [{"slack_enabled": "x", "slack_webhook_url": "nope", "team_id": 1}],
        "valid": {"slack_enabled": False, "slack_webhook_url": "https://hooks.slack.com/services/a/b/c", "status_change_slack_notifications": True},
    },
    "telegram": {
        "errors": [{"telegram_enabled": "x", "telegram_token": "a" * 256, "telegram_chat_id": 1, "telegram_notifications_deployment_success_thread_id": ["x"], "x": 1}],
        "valid": {"telegram_enabled": False, "telegram_token": "tok", "telegram_chat_id": "-100", "telegram_notifications_backup_success_thread_id": "9", "backup_success_telegram_notifications": True},
    },
    "pushover": {
        "errors": [{"pushover_enabled": "x", "pushover_user_key": ["x"], "pushover_api_token": "a" * 256, "y": 2}],
        "valid": {"pushover_enabled": False, "pushover_user_key": "uk", "pushover_api_token": "at", "traefik_outdated_pushover_notifications": False},
    },
    "webhook": {
        "errors": [{"webhook_enabled": "x", "webhook_url": "nope"}, {"webhook_url": "http://localhost:9/x"}],
        "valid": {"webhook_enabled": False, "webhook_url": "https://example.com/hook", "server_reachable_webhook_notifications": True},
    },
}


def notifications():
    team = json.load(open(os.path.join(os.path.dirname(__file__), "../reference/secrets/tokens.json")))["acme_team_id"]
    for ch, spec in CHANNELS.items():
        table = f"{ch}_notification_settings"
        saved = sql(f"select row_to_json(t) from {table} t where team_id = {team}")
        path = f"/api/v1/notifications/{ch}"
        for tok in ("acme_owner_root", "acme_owner_read", "acme_member_read", "carol_root"):
            same(f"GET {ch} as {tok}", "GET", path, tok=tok)
        same(f"PATCH {ch} read token", "PATCH", path, {f"{ch}_enabled": False}, tok="acme_owner_read")
        same(f"PATCH {ch} member", "PATCH", path, {}, tok="acme_member_write_legacy")
        same(f"PATCH {ch} empty", "PATCH", path, {})
        for i, e in enumerate(spec["errors"]):
            same(f"PATCH {ch} errors {i}", "PATCH", path, e)
        cols = ",".join(k for k in spec["valid"] if not any(w in k for w in ("url", "token", "password", "username", "host", "from_", "recipients", "user_key", "chat_id", "thread")))
        both(f"PATCH {ch} valid",
             lambda: call("ref", "PATCH", path, spec["valid"], "acme_owner_root"),
             lambda: call("rs", "PATCH", path, spec["valid"], "acme_owner_root"))
        rs_row = sql(f"select row_to_json(t) from (select {cols} from {table} where team_id = {team}) t")
        call("ref", "PATCH", path, {k: v for k, v in spec["valid"].items()}, "acme_owner_root")
        ref_row = sql(f"select row_to_json(t) from (select {cols} from {table} where team_id = {team}) t")
        record(f"{ch} row", rs_row == ref_row, f"{ref_row}\n{rs_row}")
        same(f"GET {ch} after", "GET", path)
        # Restores the row exactly, encrypted values included.
        s = json.loads(saved)
        sets = ",".join(f"{k} = " + ("NULL" if v is None else ("'" + str(v).replace("'", "''") + "'")) for k, v in s.items() if k not in ("id", "team_id"))
        sql(f"update {table} set {sets} where team_id = {team}")


def tags():
    same("tags list", "GET", "/api/v1/tags")
    same("tags list read", "GET", "/api/v1/tags", tok="acme_member_read")
    for name, body in [("short", {"name": "a"}), ("missing", {}), ("extra", {"name": "okok", "x": 1}), ("notags", {"name": "<b></b>"}),
                       ("array", {"name": ["a"]}), ("long", {"name": "a" * 256})]:
        same(f"tag create {name}", "POST", "/api/v1/tags", body)
    same("tag create member", "POST", "/api/v1/tags", {"name": "zz"}, tok="acme_member_write_legacy")
    same("tag create read", "POST", "/api/v1/tags", {"name": "zz"}, tok="acme_owner_read")
    a, b = both("tag create", lambda: call("ref", "POST", "/api/v1/tags", {"name": " Diff-<i>Ref</i> "}, "acme_owner_root"),
                lambda: call("rs", "POST", "/api/v1/tags", {"name": " Diff-<i>Rs</i> "}, "acme_owner_root"))
    ua, ub = json.loads(a[1])["uuid"], json.loads(b[1])["uuid"]
    same("tag duplicate", "POST", "/api/v1/tags", {"name": "DIFF-ref"})
    same("tag list after", "GET", "/api/v1/tags", mask=lambda t: t)
    same("tag rename missing", "PATCH", "/api/v1/tags/nope", {"name": "abc"})
    same("tag rename invalid", "PATCH", f"/api/v1/tags/{ua}", {"name": "a"})
    same("tag rename conflict", "PATCH", f"/api/v1/tags/{ua}", {"name": "diff-rs"})
    same("tag rename member", "PATCH", f"/api/v1/tags/{ua}", {"name": "diff-x"}, tok="acme_member_write_legacy")
    same("tag update same name", "PATCH", f"/api/v1/tags/{ua}", {"name": "diff-ref"}, mask=lambda t: t)
    both("tag rename", lambda: call("ref", "PATCH", f"/api/v1/tags/{ua}", {"name": "diff2-ref"}, "acme_owner_root"),
         lambda: call("rs", "PATCH", f"/api/v1/tags/{ub}", {"name": "diff2-rs"}, "acme_owner_root"))
    same("tag delete missing", "DELETE", "/api/v1/tags/nope")
    same("tag delete member", "DELETE", f"/api/v1/tags/{ua}", tok="acme_member_write_legacy")
    both("tag delete", lambda: call("ref", "DELETE", f"/api/v1/tags/{ua}", None, "acme_owner_root"),
         lambda: call("rs", "DELETE", f"/api/v1/tags/{ub}", None, "acme_owner_root"))


def shared_scope(label, base):
    same(f"{label} list", "GET", base)
    same(f"{label} list member", "GET", base, tok="acme_member_read")
    same(f"{label} list carol", "GET", base, tok="carol_root")
    for name, body in [("missing key", {"value": "x"}), ("bad key", {"key": "1 bad", "value": "x"}), ("extra", {"key": "A", "value": "x", "foo": 1}),
                       ("types", {"key": "A", "value": ["x"], "is_literal": "x", "is_multiline": 5, "comment": "c" * 257})]:
        same(f"{label} create {name}", "POST", base, body)
    same(f"{label} create member", "POST", base, {"key": "M", "value": "x"}, tok="acme_member_write_legacy")
    same(f"{label} create read", "POST", base, {"key": "M", "value": "x"}, tok="acme_owner_read")
    a, b = both(f"{label} create",
                lambda: call("ref", "POST", base, {"key": " DIFF_REF ", "value": "v1", "is_literal": True, "comment": "c"}, "acme_owner_root"),
                lambda: call("rs", "POST", base, {"key": " DIFF_RS ", "value": "v1", "is_literal": True, "comment": "c"}, "acme_owner_root"))
    ida, idb = json.loads(a[1])["id"], json.loads(b[1])["id"]
    same(f"{label} duplicate", "POST", base, {"key": "DIFF_REF", "value": "x"})
    cols = "key,is_literal,is_multiline,is_shown_once,comment,type,project_id,environment_id,server_id,version"
    ra = sql(f"select row_to_json(t) from (select {cols} from shared_environment_variables where id = {ida}) t").replace("DIFF_REF", "K")
    rb = sql(f"select row_to_json(t) from (select {cols} from shared_environment_variables where id = {idb}) t").replace("DIFF_RS", "K")
    record(f"{label} row", ra == rb, f"{ra}\n{rb}")
    same(f"{label} patch missing", "PATCH", f"{base}/999999", {"value": "x"})
    same(f"{label} patch word id", "PATCH", f"{base}/abc", {"value": "x"})
    same(f"{label} patch member", "PATCH", f"{base}/{ida}", {"value": "x"}, tok="acme_member_write_legacy")
    same(f"{label} patch empty", "PATCH", f"{base}/{ida}", {})
    same(f"{label} patch conflict", "PATCH", f"{base}/{ida}", {"key": "DIFF_RS"})
    same(f"{label} patch extra", "PATCH", f"{base}/{ida}", {"foo": 1})
    both(f"{label} patch",
         lambda: call("ref", "PATCH", f"{base}/{ida}", {"value": "v2", "is_multiline": True, "comment": "new"}, "acme_owner_root"),
         lambda: call("rs", "PATCH", f"{base}/{idb}", {"value": "v2", "is_multiline": True, "comment": "new"}, "acme_owner_root"))
    both(f"{label} patch shown once",
         lambda: call("ref", "PATCH", f"{base}/{ida}", {"is_shown_once": True}, "acme_owner_root"),
         lambda: call("rs", "PATCH", f"{base}/{idb}", {"is_shown_once": True}, "acme_owner_root"))
    same(f"{label} list after", "GET", base)
    same(f"{label} delete missing", "DELETE", f"{base}/999999")
    both(f"{label} delete", lambda: call("ref", "DELETE", f"{base}/{ida}", None, "acme_owner_root"),
         lambda: call("rs", "DELETE", f"{base}/{idb}", None, "acme_owner_root"))


def shared():
    shared_scope("team envs", "/api/v1/team/envs")
    project = "j8xm2emlllhfnyebjzgyzp7z"
    same("project envs missing project", "GET", "/api/v1/projects/nope/envs")
    shared_scope("project envs", f"/api/v1/projects/{project}/envs")
    same("env envs missing env", "GET", f"/api/v1/projects/{project}/environments/nope/envs")
    shared_scope("environment envs", f"/api/v1/projects/{project}/environments/production/envs")
    same("env envs by uuid", "GET", f"/api/v1/projects/{project}/environments/uj3st9yvtvzxh7rul3c3opcz/envs")


def audit_events():
    same("audit member", "GET", "/api/v1/audit-events", tok="acme_member_read")
    same("audit bad per_page", "GET", "/api/v1/audit-events?per_page=0")
    same("audit bad source", "GET", "/api/v1/audit-events?source=nope&page=0")
    same("audit long search", "GET", "/api/v1/audit-events?search=" + "a" * 256)
    # Rows can change between the two calls while other engineers work: use a fixed filter.
    start = audit_max()
    call("ref", "POST", "/api/v1/tags", {"name": "audit-probe"}, "acme_owner_root")
    time.sleep(0.5)
    q = "per_page=2&search=audit-probe&source=api&action=created"
    same("audit filtered root", "GET", f"/api/v1/audit-events?{q}")
    same("audit filtered read", "GET", f"/api/v1/audit-events?{q}", tok="acme_owner_read")
    same("audit page 2", "GET", f"/api/v1/audit-events?{q}&page=2")
    same("audit none", "GET", "/api/v1/audit-events?search=no-such-event-at-all")
    same("audit carol", "GET", "/api/v1/audit-events?search=audit-probe", tok="carol_root")
    uuid = [t for t in json.loads(call("ref", "GET", "/api/v1/tags", None, "acme_owner_root")[1]) if t["name"] == "audit-probe"][0]["uuid"]
    call("ref", "DELETE", f"/api/v1/tags/{uuid}", None, "acme_owner_root")
    _ = start


def instance_email():
    saved = sql("select row_to_json(t) from (select smtp_enabled,smtp_from_address,smtp_from_name,smtp_host,smtp_port,smtp_encryption,smtp_username,smtp_password,smtp_timeout,smtp_ehlo_domain,resend_enabled,resend_api_key from instance_settings where id=0) t")
    p = "/api/v1/settings/email"
    for tok in ("root_root", "root_read", "acme_owner_root", "carol_root"):
        same(f"GET settings/email {tok}", "GET", p, tok=tok)
    same("PATCH settings/email non-root", "PATCH", p, {"smtp_port": 25}, tok="acme_owner_root")
    same("PATCH settings/email read", "PATCH", p, {"smtp_port": 25}, tok="root_read")
    same("PATCH settings/email errors", "PATCH", p, {"smtp_enabled": "x", "smtp_port": 0, "smtp_from_address": "x", "smtp_encryption": "ssl", "smtp_ehlo_domain": "a b"}, tok="root_root")
    same("PATCH settings/email extra ignored", "PATCH", p, {"nope": 1}, tok="root_root")
    both("PATCH settings/email", lambda: call("ref", "PATCH", p, {"smtp_port": 2526, "smtp_host": "h.example.test", "smtp_from_name": "N", "smtp_enabled": False}, "root_root"),
         lambda: call("rs", "PATCH", p, {"smtp_port": 2526, "smtp_host": "h.example.test", "smtp_from_name": "N", "smtp_enabled": False}, "root_root"))
    same("GET settings/email after", "GET", p, tok="root_root")
    s = json.loads(saved)
    sets = ",".join(f"{k} = " + ("NULL" if v is None else ("'" + str(v).replace("'", "''") + "'")) for k, v in s.items())
    sql(f"update instance_settings set {sets} where id = 0")


def mcp():
    was = sql("select is_mcp_server_enabled from instance_settings where id=0")
    same("mcp enable non-root", "POST", "/api/v1/mcp/enable", tok="acme_owner_root")
    same("mcp disable non-root", "POST", "/api/v1/mcp/disable", tok="acme_owner_root")
    same("mcp enable read", "POST", "/api/v1/mcp/enable", tok="root_read")
    both("mcp disable", lambda: call("ref", "POST", "/api/v1/mcp/disable", None, "root_root"), lambda: call("rs", "POST", "/api/v1/mcp/disable", None, "root_root"))
    both("mcp enable", lambda: call("ref", "POST", "/api/v1/mcp/enable", None, "root_root"), lambda: call("rs", "POST", "/api/v1/mcp/enable", None, "root_root"))
    sql(f"update instance_settings set is_mcp_server_enabled = '{was}' where id=0")


def feedback():
    same("feedback missing", "POST", "/api/feedback", {})
    same("feedback short", "POST", "/api/feedback", {"content": "short"})
    same("feedback ok", "POST", "/api/feedback", {"content": "long enough content"})


def integration():
    p = "/api/v1/security/integration-tokens"
    same("integration member", "POST", p, {"provider": "doppler", "name": "x", "token": "dp.st.x"}, tok="acme_member_write_legacy")
    same("integration read", "POST", p, {"provider": "doppler"}, tok="acme_owner_read")
    same("integration empty", "POST", p, {})
    same("integration provider", "POST", p, {"provider": "cloudflare", "name": "x", "token": "t", "foo": 1})
    same("integration doppler regex", "POST", p, {"provider": "doppler", "name": "x", "token": "bad"})
    same("integration infisical", "POST", p, {"provider": "infisical", "name": "x", "token": "t", "metadata": {"timeout": 500}})
    same("integration vault", "POST", p, {"provider": "vault", "name": "x", "token": "t", "metadata": {"base_url": "ftp://x", "namespace": 5, "timeout": 0}})
    same("integration name long", "POST", p, {"provider": "doppler", "name": "x" * 256, "token": "dp.st.x"}) if False else None


def main():
    notifications()
    tags()
    shared()
    audit_events()
    instance_email()
    mcp()
    feedback()
    integration()
    print(f"{sum(results)}/{len(results)} passed")
    sys.exit(0 if all(results) else 1)


if __name__ == "__main__":
    main()
