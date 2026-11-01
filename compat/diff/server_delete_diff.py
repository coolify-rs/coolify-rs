#!/usr/bin/env python3
"""Differential of `ServersController::delete_server`: every refusal, then `?force=true` for real on the VM.

    COOLIFY_RS_BASE=http://127.0.0.1:18088 python3 compat/diff/server_delete_diff.py [errors] [force]

Each side gets its own throwaway server row on the test VM's address, with a database it started, an
application and a service. No request here reaches a cloud provider: every provider case stops at a refusal
before `DeleteServer` is dispatched. Rows and VM paths are tracked by uuid and removed at the end.
"""
import base64
import json
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from databases_diff import BASES, call, record, results, same, sql  # noqa: E402

VM = "coolify-rs-e2e"
VM_IP = "192.168.139.220"
KEY = "hv41morfjv9x0av5kakzdmv8"
ROOT = "acme_owner_root"
COMPOSE = "services:\n  web:\n    image: nginx:alpine\n"
tracked = {"servers": [], "tokens": [], "projects": []}


def vm(cmd):
    return subprocess.run(["orb", "-m", VM, "-u", "root", "bash", "-c", cmd], capture_output=True, text=True).stdout.strip()


def js(r):
    return json.loads(r[1])


def new_server(name):
    st, body = call("ref", "POST", "/api/v1/servers", {"name": name, "ip": VM_IP, "port": 22, "user": "root", "private_key_uuid": KEY, "instant_validate": False}, ROOT)
    assert st == 201, (st, body)
    u = json.loads(body)["uuid"]
    tracked["servers"].append(u)
    sid = sql(f"select id from servers where uuid='{u}'")
    sql(f"update server_settings set is_reachable=true, is_usable=true where server_id={sid}")
    return u, sid


def drop_server(u):
    sid = sql(f"select id from servers where uuid='{u}'")
    if not sid:
        return
    for q in (
        f"delete from standalone_dockers where server_id={sid}",
        f"delete from swarm_dockers where server_id={sid}",
        f"delete from server_settings where server_id={sid}",
        f"delete from ssl_certificates where server_id={sid}",
        f"delete from docker_cleanup_executions where server_id={sid}",
        f"delete from servers where id={sid}",
    ):
        sql(q)


def errors():
    u, sid = new_server("s2-delete-errors")
    c, r = call("ref", "POST", "/api/v1/projects", {"name": "s2-delete-errors"}, ROOT)
    project = json.loads(r)["uuid"]
    tracked["projects"].append(project)
    env = js(call("ref", "GET", f"/api/v1/projects/{project}", None, ROOT))["environments"][0]["uuid"]
    same("missing", "DELETE", "/api/v1/servers/nope", None, ROOT)
    same("member", "DELETE", f"/api/v1/servers/{u}", None, "acme_member_write_legacy")
    same("read token", "DELETE", f"/api/v1/servers/{u}", None, "acme_owner_read")
    same("write token bad flag", "DELETE", f"/api/v1/servers/{u}?delete_from_provider=maybe", None, ROOT)
    same("not linked", "DELETE", f"/api/v1/servers/{u}?delete_from_provider=true", None, ROOT)
    same("local server", "DELETE", "/api/v1/servers/" + sql("select uuid from servers where id=0"), None, "root_root")
    # With a resource.
    c, r = call("ref", "POST", "/api/v1/databases/redis", {"project_uuid": project, "server_uuid": u, "environment_uuid": env, "instant_deploy": False, "name": "s2-del-err"}, ROOT)
    assert c in (200, 201), (c, r)
    db = json.loads(r)["uuid"]
    same("has resources", "DELETE", f"/api/v1/servers/{u}", None, ROOT)
    same("has resources force=no", "DELETE", f"/api/v1/servers/{u}?force=no", None, ROOT)
    same("has resources not linked", "DELETE", f"/api/v1/servers/{u}?force=true&delete_from_provider=true", None, ROOT)
    same("force needs deploy", "DELETE", f"/api/v1/servers/{u}?force=true", None, "acme_owner_write")
    call("ref", "DELETE", f"/api/v1/databases/{db}?delete_configurations=false&delete_volumes=false&docker_cleanup=false&delete_connected_networks=false", None, ROOT)
    for _ in range(30):
        if not sql(f"select id from standalone_redis where uuid='{db}'"):
            break
        time.sleep(1)
    # Linked to a provider. The token row exists only for these refusals: nothing reads its value.
    sql(f"update servers set vultr_instance_id='screens2-fake' where id={sid}")
    same("linked no token", "DELETE", f"/api/v1/servers/{u}?delete_from_provider=true", None, ROOT)
    token_uuid = "screens2fakecloudtoken00"
    if sql("select count(*) from cloud_provider_tokens where team_id=4 and provider='vultr'") == "0":
        tid = sql(f"insert into cloud_provider_tokens (team_id,provider,token,name,uuid,created_at,updated_at) values (4,'vultr','unused','screens2-fake','{token_uuid}',now(),now()) returning id").splitlines()[0]
        tracked["tokens"].append(tid)
        same("linked needs deploy", "DELETE", f"/api/v1/servers/{u}?delete_from_provider=true", None, "acme_owner_write")
    sql(f"update servers set vultr_instance_id=null where id={sid}")
    # A Hostinger server is not a linked provider for the API.
    sql(f"update servers set hostinger_virtual_machine_id=1 where id={sid}")
    same("hostinger not linked", "DELETE", f"/api/v1/servers/{u}?delete_from_provider=true", None, ROOT)
    sql(f"update servers set hostinger_virtual_machine_id=null where id={sid}")
    drop_server(u)


def force_one(side):
    u, sid = new_server(f"s2-force-{side}")
    c, r = call("ref", "POST", "/api/v1/projects", {"name": f"s2-force-{side}"}, ROOT)
    project = json.loads(r)["uuid"]
    tracked["projects"].append(project)
    env = js(call("ref", "GET", f"/api/v1/projects/{project}", None, ROOT))["environments"][0]["uuid"]
    common = {"project_uuid": project, "server_uuid": u, "environment_uuid": env, "instant_deploy": False}
    c, r = call("ref", "POST", "/api/v1/databases/redis", dict(common, name="s2-force-db"), ROOT)
    assert c in (200, 201), (c, r)
    db = json.loads(r)["uuid"]
    c, r = call("ref", "POST", "/api/v1/applications/dockerimage", dict(common, name="s2-force-app", docker_registry_image_name="nginx", ports_exposes="80"), ROOT)
    assert c == 201, (c, r)
    app = json.loads(r)["uuid"]
    c, r = call("ref", "POST", "/api/v1/services", dict(common, name="s2-force-svc", docker_compose_raw=base64.b64encode(COMPOSE.encode()).decode()), ROOT)
    assert c == 201, (c, r)
    svc = json.loads(r)["uuid"]
    # A running container, started by the side under test.
    c, r = call(side, "POST", f"/api/v1/databases/{db}/start", None, ROOT)
    assert c == 200, (c, r)
    for _ in range(60):
        if db in vm("docker ps --format '{{.Names}}'"):
            break
        time.sleep(2)
    before = db in vm("docker ps --format '{{.Names}}'")
    dep_app = sql(f"select id from applications where uuid='{app}'")
    queued = sql(f"insert into application_deployment_queues (application_id,deployment_uuid,status,server_id,application_name,server_name,created_at,updated_at,pull_request_id,force_rebuild,restart_only,build_server_id,only_this_server,rollback,commit,destination_id) values ('{dep_app}','screens2queued{side}','queued',{sid},'s2-force-app','s2','now','now',0,false,false,null,false,false,'HEAD',null) returning id").splitlines()[0]
    max_audit = sql("select coalesce(max(id),0) from audit_events")
    status, body = call(side, "DELETE", f"/api/v1/servers/{u}?force=true", None, ROOT)
    at_response = sql(f"select deleted_at is not null from servers where id={sid}")
    gone = {}
    for _ in range(90):
        gone = {
            "server": not sql(f"select id from servers where id={sid}"),
            "db": not sql(f"select id from standalone_redis where uuid='{db}'"),
            "app": not sql(f"select id from applications where uuid='{app}'"),
            "svc": not sql(f"select id from services where uuid='{svc}'"),
        }
        if all(gone.values()):
            break
        time.sleep(2)
    time.sleep(5)
    out = {
        "status": status,
        "body": body.decode(),
        "before": before,
        "soft_deleted_or_gone_at_response": at_response in ("t", ""),
        "rows_gone": gone,
        "queued_status": sql(f"select status from application_deployment_queues where id={queued}"),
        "settings": sql(f"select count(*) from server_settings where server_id={sid}"),
        "dockers": sql(f"select count(*) from standalone_dockers where server_id={sid}"),
        "container_left": db in vm("docker ps -a --format '{{.Names}}'"),
        "volume_left": db in vm("docker volume ls --format '{{.Name}}'"),
        "dir_left": vm(f"test -d /data/coolify/databases/{db} && echo yes || echo no"),
        "audit": sql(f"select event, metadata->>'force', metadata->>'delete_from_provider', metadata->>'server_uuid' = '{u}' from audit_events where id>{max_audit} and event='api.server.deleted'"),
        "notes": sql(f"select count(*) from application_deployment_queues where id={queued}"),
    }
    # Whatever a side left behind for this resource.
    vm(f"docker rm -f $(docker ps -aq --filter name={db}) 2>/dev/null; docker volume rm -f $(docker volume ls -q --filter name={db}) 2>/dev/null; rm -rf /data/coolify/databases/{db} /data/coolify/applications/{app} /data/coolify/services/{svc}")
    sql(f"delete from application_deployment_queues where id={queued}")
    print(side, json.dumps(out))
    return out


def force():
    a, b = force_one("ref"), force_one("rs")
    for k in a:
        record(f"force {k}", a[k] == b[k], f"ref {a[k]} rs {b[k]}")


def main():
    which = sys.argv[1:] or ["errors", "force"]
    try:
        if "errors" in which:
            errors()
        if "force" in which:
            force()
    finally:
        for t in tracked["tokens"]:
            sql(f"delete from cloud_provider_tokens where id={t}")
        for u in tracked["servers"]:
            drop_server(u)
        for p in tracked["projects"]:
            call("ref", "DELETE", f"/api/v1/projects/{p}", None, ROOT)
    print(f"{sum(results)}/{len(results)} same; rs base {BASES['rs']}")
    sys.exit(0 if all(results) else 1)


main()
