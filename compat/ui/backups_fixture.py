#!/usr/bin/env python3
"""Rows behind flow_backups.mjs, shared by both sides. Usage:

    backups_fixture.py create > /path/fixture.json
    backups_fixture.py delete /path/fixture.json

Services and applications go through the reference API and are never started; schedules and executions are rows with
fixed timestamps, so both sides show the same relative times. Everything is deleted by the ids in the fixture file.
"""
import json
import os
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "../diff"))
import databases_diff as d  # noqa: E402

SERVER = "hml20fsj5eles0xfiqidq8j5"
PROJECT = "j8xm2emlllhfnyebjzgyzp7z"
ENV = "uj3st9yvtvzxh7rul3c3opcz"
ROOT = "acme_owner_root"
Q = "?delete_volumes=true&delete_configurations=true&docker_cleanup=true"


def service(name, compose):
    c, b = d.call("ref", "POST", "/api/v1/services", {"server_uuid": SERVER, "project_uuid": PROJECT, "environment_uuid": ENV, "name": name, "docker_compose_raw": d.b64(compose), "instant_deploy": False}, tok=ROOT)
    assert c < 300, b
    return json.loads(b)["uuid"]


def application(name):
    c, b = d.call("ref", "POST", "/api/v1/applications/dockerimage", {"server_uuid": SERVER, "project_uuid": PROJECT, "environment_uuid": ENV, "name": name, "docker_registry_image_name": "nginx", "docker_registry_image_tag": "latest", "ports_exposes": "80", "instant_deploy": False}, tok=ROOT)
    assert c < 300, b
    return json.loads(b)["uuid"]


def row(q):
    t = d.sql(f"select row_to_json(x) from ({q}) x")
    return json.loads(t) if t else None


def create():
    stamp = int(time.time())
    fx = {"project": PROJECT, "env": ENV}
    fx["svc"] = service("wave5-svc", "services:\n  db:\n    image: postgres:16\n    environment:\n      POSTGRES_PASSWORD: a\n    volumes:\n      - dbdata:/var/lib/postgresql/data\n  web:\n    image: nginx:latest\n    volumes:\n      - webdata:/data\n")
    fx["svc_none"] = service("wave5-none", "services:\n  web:\n    image: nginx:latest\n")
    fx["svc_two"] = service("wave5-two", "services:\n  pg:\n    image: postgres:16\n    environment:\n      POSTGRES_PASSWORD: a\n  my:\n    image: mysql:8\n    environment:\n      MYSQL_ROOT_PASSWORD: a\n")
    fx["app"] = application("wave5-app")
    fx["app_empty"] = application("wave5-app-empty")
    database = row(f"select d.id, d.uuid from service_databases d join services s on s.id = d.service_id where s.uuid = '{fx['svc']}'")
    web = row(f"select a.id from service_applications a join services s on s.id = a.service_id where s.uuid = '{fx['svc']}'")
    web_volume = row(f"select id from local_persistent_volumes where resource_type = 'App\\Models\\ServiceApplication' and resource_id = {web['id']}")
    app_id = d.sql(f"select id from applications where uuid = '{fx['app']}'")
    fx["database"] = database["uuid"]
    team = d.sql(f"select team_id from projects where uuid = '{PROJECT}'")
    fx["app_volume_uuid"] = f"wave5vol{stamp}"
    fx["app_volume"] = int(d.sql(
        f"insert into local_persistent_volumes (uuid, name, mount_path, resource_type, resource_id, created_at, updated_at) values ('{fx['app_volume_uuid']}', 'wave5-appdata', '/data', 'App\\Models\\Application', {app_id}, now(), now()) returning id"
    ).split("\n")[0])
    fx["app_volume2"] = int(d.sql(
        f"insert into local_persistent_volumes (uuid, name, mount_path, resource_type, resource_id, created_at, updated_at) values ('{fx['app_volume_uuid']}b', 'wave5-unscheduled', '/more', 'App\\Models\\Application', {app_id}, now(), now()) returning id"
    ).split("\n")[0])
    fx["web_backup"] = f"wave5web{stamp}"
    fx["app_backup"] = f"wave5app{stamp}"
    fx["db_backup"] = f"wave5db{stamp}"
    created = "'2026-01-02 03:04:05'"
    d.sql(f"insert into scheduled_volume_backups (uuid, team_id, backupable_type, backupable_id, frequency, enabled, created_at, updated_at) values ('{fx['web_backup']}', {team}, 'App\\Models\\LocalPersistentVolume', {web_volume['id']}, 'daily', true, {created}, {created})")
    d.sql(f"insert into scheduled_volume_backups (uuid, team_id, backupable_type, backupable_id, frequency, enabled, created_at, updated_at) values ('{fx['app_backup']}', {team}, 'App\\Models\\LocalPersistentVolume', {fx['app_volume']}, '0 2 * * *', true, {created}, {created})")
    d.sql(f"insert into scheduled_database_backups (uuid, team_id, enabled, frequency, save_s3, database_type, database_id, created_at, updated_at) values ('{fx['db_backup']}', {team}, true, 'hourly', false, 'App\\Models\\ServiceDatabase', {database['id']}, {created}, {created})")
    web_id = d.sql(f"select id from scheduled_volume_backups where uuid = '{fx['web_backup']}'")
    app_backup_id = d.sql(f"select id from scheduled_volume_backups where uuid = '{fx['app_backup']}'")
    db_id = d.sql(f"select id from scheduled_database_backups where uuid = '{fx['db_backup']}'")
    for schedule, uuid, status, message, filename, size in (
        (web_id, f"wave5e1{stamp}", "success", None, "/data/coolify/backups/volumes/acme-4/x/volume-webdata-1767322000.tar.gz", 1048576),
        (app_backup_id, f"wave5e2{stamp}", "success", None, "/data/coolify/backups/volumes/acme-4/y/volume-wave5-appdata-1767322000.tar.gz", 2048),
        (app_backup_id, f"wave5e3{stamp}", "failed", "Could not archive the volume.", None, 0),
    ):
        d.sql(
            "insert into scheduled_volume_backup_executions (uuid, scheduled_volume_backup_id, status, message, filename, size, finished_at, created_at, updated_at) "
            f"values ('{uuid}', {schedule}, '{status}', {repr(message) if message else 'null'}, {repr(filename) if filename else 'null'}, {size}, '2026-01-02 04:00:30', '2026-01-02 04:00:00', '2026-01-02 04:00:30')"
        )
    d.sql(
        "insert into scheduled_database_backup_executions (uuid, scheduled_database_backup_id, status, message, filename, size, database_name, finished_at, created_at, updated_at) "
        f"values ('wave5e4{stamp}', {db_id}, 'success', 'pg_dump output', '/data/coolify/backups/databases/acme-4/s/pg-dump-postgres-1767322000-a.dmp', 4096, 'postgres', '2026-01-02 05:00:30', '2026-01-02 05:00:00', '2026-01-02 05:00:30')"
    )
    print(json.dumps(fx))


def delete(path):
    fx = json.load(open(path))
    for uuid in (fx["web_backup"], fx["app_backup"]):
        d.sql(f"delete from scheduled_volume_backup_executions where scheduled_volume_backup_id in (select id from scheduled_volume_backups where uuid = '{uuid}')")
        d.sql(f"delete from scheduled_volume_backups where uuid = '{uuid}'")
    d.sql(f"delete from scheduled_database_backup_executions where scheduled_database_backup_id in (select id from scheduled_database_backups where uuid = '{fx['db_backup']}')")
    d.sql(f"delete from scheduled_database_backups where uuid = '{fx['db_backup']}'")
    # Schedules a flow made on these resources go with their volumes, and only those.
    for volume in (fx["app_volume"], fx["app_volume2"]):
        d.sql(f"delete from scheduled_volume_backups where backupable_type = 'App\\Models\\LocalPersistentVolume' and backupable_id = {volume}")
        d.sql(f"delete from local_persistent_volumes where id = {volume}")
    for name in ("svc", "svc_none", "svc_two"):
        print(name, d.call("ref", "DELETE", f"/api/v1/services/{fx[name]}{Q}", tok=ROOT)[0])
    for name in ("app", "app_empty"):
        print(name, d.call("ref", "DELETE", f"/api/v1/applications/{fx[name]}{Q}", tok=ROOT)[0])


if sys.argv[1] == "create":
    create()
else:
    delete(sys.argv[2])
