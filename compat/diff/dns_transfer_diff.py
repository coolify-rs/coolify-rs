#!/usr/bin/env python3
"""`adoptRecord` in the transfer import and `reclaimRecord` in the finish list, on both sides, against the fake.

    COOLIFY_RS_BASE=http://127.0.0.1:18087 python3 compat/diff/dns_transfer_diff.py

The reference runs under tinker with Http::fake. Every row is tracked and removed at the end.
"""
import base64
import hashlib
import hmac
import json
import re
import sys
import urllib.request

from dns_flows_diff import FAKE, add_zone, cleanup, create_server, create_token, created, guard_hour, lw_prelude
from integrations_diff import ENVIRONMENT, RS, compare, fake, fake_log, login, norm_requests, record, results, sql, team_id, tinker
from transfer_diff import bundle, call, delete_import

UNREACHABLE = "10.255.255.8"


def provider_record(name, comment):
    body = {"type": "A", "name": name, "content": "9.9.9.9", "comment": comment}
    req = urllib.request.Request(FAKE + "/cloudflare/client/v4/zones/zone-example/dns_records", method="POST", data=json.dumps(body).encode())
    req.add_header("Authorization", "Bearer cf-good")
    with urllib.request.urlopen(req) as r:
        return json.loads(r.read())["result"]["id"]


def norm(requests):
    out = norm_requests(requests)
    for e in out:
        if isinstance(e.get("body"), dict) and e["body"].get("comment"):
            e["body"]["comment"] = re.sub(r"coolify \S+/\S+", "coolify ID/UUID", e["body"]["comment"])
    return out


def rows(tid):
    return json.loads(sql(f"SELECT coalesce(json_agg(json_build_object('name', name, 'owned', owned) ORDER BY name), '[]') FROM managed_dns_records WHERE integration_token_id = {tid}") or "[]")


def reclaim(ui, srv):
    tid = create_token("dns2-reclaim")
    add_zone(tid)
    zone = sql(f"SELECT id FROM dns_provider_zones WHERE integration_token_id = {tid}")

    def seed(comment_of):
        sql(f"DELETE FROM managed_dns_records WHERE integration_token_id = {tid}")
        fake("/__reset", "POST")
        uuid = "dns2rec" + comment_of[:3]
        comment = {"own": f"managed-by: coolify other/{uuid}", "bad": "somebody else"}[comment_of]
        rid = provider_record("taken.example.com", comment)
        sql(f"INSERT INTO managed_dns_records (uuid, team_id, integration_token_id, dns_provider_zone_id, provider_record_id, type, name, content, owned, created_at, updated_at) "
            f"VALUES ('{uuid}', {team_id()}, {tid}, {zone}, '{rid}', 'A', 'taken.example.com', '9.9.9.9', false, now(), now())")
        key = f"dns_record:{uuid}"
        meta = {"transfer": {"finish": {"started_at": "2026-11-01T00:00:00+00:00", "items": [{"key": key, "type": "dns_record", "subject": uuid, "title": "DNS record taken.example.com", "group": "dns", "status": "action", "message": "", "error": None, "instructions": None, "details": [], "link": None, "link_label": None}]}}}
        sql(f"UPDATE servers SET server_metadata = '{json.dumps(meta)}' WHERE id = {srv['id']}")
        return key, len(fake_log())

    for label in ("own", "bad"):
        key, before = seed(label)
        php = lw_prelude() + f"""
$s = \\App\\Models\\Server::find({srv['id']});
try {{ $r = app(\\App\\Services\\ServerTransfer\\TransferFinishChecklist::class)->fix($s, {json.dumps(key)}); }} catch (\\Throwable $e) {{ $r = ['thrown' => $e->getMessage()]; }}
echo '<<<'.json_encode($r).'>>>';"""
        ref = tinker(php)
        ref_req, ref_rows = norm(fake_log()[before:]), rows(tid)
        key, before = seed(label)
        code, rs = ui("POST", f"/ui/server-page/{srv['uuid']}/transfer-finish/fix", {"key": key})
        rs_req, rs_rows = norm(fake_log()[before:]), rows(tid)
        compare(f"reclaim {label}: status", (ref.get("status"), ref.get("message"), ref.get("error")), (rs.get("status"), rs.get("message"), rs.get("error")))
        compare(f"reclaim {label}: requests", ref_req, rs_req)
        compare(f"reclaim {label}: rows", ref_rows, rs_rows)
    sql(f"UPDATE servers SET server_metadata = NULL WHERE id = {srv['id']}")


def receipts(ui, srv):
    """recordWebhook: the signed manual GitHub webhook and the finish list step. The server is not usable, so nothing deploys."""
    php = f"""
$e = \\App\\Models\\Environment::where('uuid', '{ENVIRONMENT}')->first();
$a = new \\App\\Models\\Application(['name' => 'dns2-hook', 'repository_project_id' => 0, 'git_repository' => 'dns2/webhook', 'git_branch' => 'main', 'build_pack' => 'dockerimage', 'ports_exposes' => '80', 'environment_id' => $e->id, 'destination_id' => {srv['dest']}, 'destination_type' => \\App\\Models\\StandaloneDocker::class, 'docker_registry_image_name' => 'nginx', 'docker_registry_image_tag' => 'alpine']);
$a->manual_webhook_secret_github = 'dns2-hook-secret';
$a->save();
echo '<<<'.json_encode(['id' => $a->id, 'uuid' => $a->uuid]).'>>>';"""
    app = tinker(php, fake=False)
    created["apps"].append(app["id"])
    key = f"manual_webhook:{app['uuid']}"

    def seed():
        meta = {"transfer": {"finish": {"started_at": "2026-11-01T00:00:00+00:00", "items": [{"key": key, "type": "manual_webhook", "subject": app["uuid"], "title": "Manual webhook", "group": "git", "status": "action", "message": "", "error": None, "instructions": None, "details": [], "link": None, "link_label": None}]}}}
        sql(f"UPDATE servers SET server_metadata = '{json.dumps(meta)}' WHERE id = {srv['id']}")

    def hook(base):
        body = json.dumps({"ref": "refs/heads/main", "repository": {"full_name": "dns2/webhook"}, "commits": [], "head_commit": {"id": "a" * 40, "message": "m"}, "after": "a" * 40, "before": "b" * 40}).encode()
        sign = hmac.new(b"dns2-hook-secret", body, hashlib.sha256).hexdigest()
        req = urllib.request.Request(base + "/webhooks/source/github/events/manual", data=body, method="POST")
        for k, v in {"Content-Type": "application/json", "X-GitHub-Event": "push", "X-Hub-Signature-256": "sha256=" + sign}.items():
            req.add_header(k, v)
        with urllib.request.urlopen(req) as r:
            return r.read().decode()

    def ref_check():
        php = lw_prelude() + f"""$s = \\App\\Models\\Server::find({srv['id']}); $r = app(\\App\\Services\\ServerTransfer\\TransferFinishChecklist::class)->check($s, {json.dumps(key)}); echo '<<<'.json_encode($r).'>>>';"""
        return tinker(php)

    def rs_check():
        return ui("POST", f"/ui/server-page/{srv['uuid']}/transfer-finish/check", {"key": key})[1]

    view = lambda r: (r.get("status"), re.sub(r"\d\d:\d\d", "HH:MM", r.get("message") or ""))
    seed()
    ref_before = view(ref_check())
    ref_answer = hook("http://127.0.0.1:18000")
    ref_after = view(ref_check())
    print("reference answers", ref_answer[:200])
    seed()
    rs_before = view(rs_check())
    rs_answer = hook(RS)
    rs_after = view(rs_check())
    compare("receipt: before a webhook", ref_before, rs_before)
    compare("receipt: the webhook answers", ref_answer, rs_answer)
    compare("receipt: after a webhook", ref_after, rs_after)
    record("receipt: it turned ok", rs_after[0] == "ok", rs_after)
    sql(f"UPDATE servers SET server_metadata = NULL WHERE id = {srv['id']}")


def adoption():
    fake("/__reset", "POST")
    good = "managed-by: coolify source/dns2ta" + "rg"
    results_by_side = {}
    for side in ("ref", "rs"):
        uuid = "dns2t" + side[0]
        ip = UNREACHABLE + ("1" if side == "ref" else "2")
        fake("/__reset", "POST")
        rec_ok = provider_record("adopted.example.com", good)
        rec_bad = provider_record("refused.example.com", "changed by hand")
        token_uuid, zone_uuid = uuid + "tok", uuid + "zon"
        extra = {
            "integration_tokens": [{"uuid": token_uuid, "provider": "cloudflare", "name": "dns2 imported", "token": "cf-good", "capabilities": ["dns"], "metadata": {}}],
            "dns_zones": [{"uuid": zone_uuid, "integration_token_uuid": token_uuid, "provider_zone_id": "zone-example", "name": "example.com", "account_id": "acct-1"}],
            "managed_dns_records": [
                {"uuid": uuid + "r1", "dns_provider_zone_uuid": zone_uuid, "provider_record_id": rec_ok, "type": "A", "name": "adopted.example.com", "content": "9.9.9.9", "owned": True, "ownership_comment": good, "references": [{"kind": "server", "uuid": uuid + "srv"}]},
                {"uuid": uuid + "r2", "dns_provider_zone_uuid": zone_uuid, "provider_record_id": rec_bad, "type": "A", "name": "refused.example.com", "content": "9.9.9.9", "owned": True, "ownership_comment": good, "references": [{"kind": "server", "uuid": uuid + "srv"}]},
            ],
        }
        b = bundle(uuid, ip=ip, **extra)
        delete_import(uuid, ip)
        tid = None
        try:
            before = len(fake_log())
            if side == "ref":
                encoded = base64.b64encode(json.dumps(b).encode()).decode()
                php = lw_prelude() + f"""
\\Illuminate\\Support\\Facades\\Bus::fake();
try {{ $r = app(\\App\\Services\\ServerTransfer\\ServerTransferImporter::class)->import(json_decode(base64_decode('{encoded}'), true), {team_id()}); }} catch (\\Throwable $e) {{ $r = ['thrown' => $e->getMessage()]; }}
echo '<<<'.json_encode(['warnings' => $r['warnings'] ?? null, 'thrown' => $r['thrown'] ?? null]).'>>>';"""
                answer = tinker(php)
            else:
                status, body = call("rs", "POST", "/api/v1/servers/import", {"bundle": b})
                answer = {"warnings": body.get("warnings") if isinstance(body, dict) else None, "thrown": None if status < 300 else body}
            reqs = [e for e in norm(fake_log()[before:]) if "dns_records" in e["path"]]
            tid = sql(f"SELECT id FROM integration_tokens WHERE uuid = '{token_uuid}'")
            state = rows(tid) if tid else None
            results_by_side[side] = (answer, reqs, state)
        finally:
            if tid:
                sql(f"DELETE FROM managed_dns_record_references WHERE managed_dns_record_id IN (SELECT id FROM managed_dns_records WHERE integration_token_id = {tid})")
                sql(f"DELETE FROM managed_dns_records WHERE integration_token_id = {tid}")
                sql(f"DELETE FROM dns_provider_zones WHERE integration_token_id = {tid}")
                sql(f"DELETE FROM integration_tokens WHERE id = {tid}")
            delete_import(uuid, ip)
    ref, rs = results_by_side["ref"], results_by_side["rs"]
    record("adoption: the reference imported", ref[0]["thrown"] is None, ref[0])
    record("adoption: coolify-rs imported", rs[0]["thrown"] is None, rs[0])
    only = lambda w: sorted(x for x in (w or []) if "DNS record" in x)
    compare("adoption: warnings", only(ref[0]["warnings"]), only(rs[0]["warnings"]))
    key = lambda e: json.dumps(e, sort_keys=True)
    compare("adoption: requests", sorted(ref[1], key=key), sorted(rs[1], key=key))
    compare("adoption: rows", ref[2], rs[2])


def main():
    guard_hour()
    ui = login()
    try:
        srv = create_server()
        reclaim(ui, srv)
        receipts(ui, srv)
        adoption()
    finally:
        cleanup()
    bad = [n for n, ok in results if not ok]
    print(f"{len(results) - len(bad)}/{len(results)} same")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
