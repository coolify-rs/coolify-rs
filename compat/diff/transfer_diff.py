#!/usr/bin/env python3
"""Differential of `ServerTransferController`: check, import (dry run, errors, real rows), status,
complete and migrate errors, and the bundle `ServerTransferExporter` writes.

Run: COOLIFY_RS_BASE=http://127.0.0.1:18088 python3 compat/diff/transfer_diff.py [section...]

Rows are created by import on one side, read, and deleted by id before the other side imports the
same bundle, because both sides share one database. Only rows this script created are removed.
No provider is contacted: the bundles have no cloud server ids and no DNS records with an owner.
"""
import json
import os
import re
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from databases_diff import BASES, ROOT, TOKENS, record, sql  # noqa: E402
from databases_diff import call as raw_call  # noqa: E402


def call(side, method, path, body=None, tok="acme_owner_root"):
    status, data = raw_call(side, method, path, body, tok)
    try:
        return status, json.loads(data)
    except ValueError:
        return status, data.decode(errors="replace")[:400]


KEY_PATH = os.path.join(os.path.realpath(os.path.join(ROOT, ".upstream")), "..", "compat/testing-host/secrets/id_ed25519")
TEAM = TOKENS["acme_team_id"]
UNREACHABLE_IP = "198.51.100.77"


def team_key():
    return open(KEY_PATH).read()


def norm(v):
    """Removes what differs between runs: generated ids, times and our version text."""
    text = json.dumps(v, sort_keys=True)
    text = re.sub(r'"(claim_id|claimed_at|imported_at|transferred_at|seen_at|at|started_at|finished_at|exported_at|export_id)": "[^"]*"', r'"\1": "X"', text)
    text = text.replace("http://127.0.0.1:18000", "URL").replace("http://localhost", "URL")
    return json.loads(text)


def paths(a, b, at=""):
    """The paths where two values differ."""
    if isinstance(a, dict) and isinstance(b, dict):
        for k in sorted(set(a) | set(b)):
            if k not in a or k not in b:
                yield f"{at}.{k}: only in {'ref' if k in a else 'rs'} = {json.dumps(a.get(k, b.get(k)))[:80]}"
            else:
                yield from paths(a[k], b[k], f"{at}.{k}")
    elif isinstance(a, list) and isinstance(b, list) and len(a) == len(b):
        for i, (x, y) in enumerate(zip(a, b)):
            yield from paths(x, y, f"{at}[{i}]")
    elif a != b:
        yield f"{at}: ref={json.dumps(a)[:110]} rs={json.dumps(b)[:110]}"


def compare(name, a, b):
    ok = a == b
    detail = "" if ok else "\n  " + "\n  ".join(list(paths(a, b))[:25])
    record(name, ok, detail)


def same(name, method, path, body=None, tok="acme_owner_root"):
    ra = call("ref", method, path, body, tok)
    rb = call("rs", method, path, body, tok)
    compare(name, (ra[0], norm(ra[1])), (rb[0], norm(rb[1])))


def bundle(uuid, ip="th-root", port=22, **over):
    u = lambda s: f"{uuid}{s}"
    b = {
        "schema_version": 1,
        "coolify_version": "4.0.0",
        "exported_at": "2026-10-31T00:00:00+00:00",
        "export_id": u("exp"),
        "source_instance": {"url": "https://source.example.test", "name": "src"},
        "warnings": ["a warning of the source"],
        "post_transfer_checklist": ["Point {target_url}/webhooks to the target.", 7],
        "private_key": {"uuid": u("key"), "name": u(" Transfer key"), "description": "d", "private_key": team_key(), "is_git_related": False, "fingerprint": "ignored"},
        "private_keys": [{"uuid": u("key"), "name": u(" Transfer key"), "description": "d", "private_key": team_key(), "is_git_related": False}],
        "github_apps": [{
            "uuid": u("gh"), "name": u(" gh app"), "organization": "org", "api_url": "https://api.github.com", "html_url": "https://github.com",
            "custom_user": "git", "custom_port": 22, "app_id": 12, "installation_id": 34, "client_id": "cid", "client_secret": "csec",
            "webhook_secret": "wsec", "is_system_wide": False, "is_public": False, "contents": "read", "metadata": "read",
            "pull_requests": "write", "administration": None, "organization_self_hosted_runners": None, "actions": None,
            "webhook_events": ["push"], "runner_group_id": None, "private_key_uuid": u("key"),
        }],
        "gitlab_apps": [], "gitea_apps": [],
        "s3_storages": [{"uuid": u("s3"), "name": "s3", "description": None, "region": "us-east-1", "key": "k", "secret": "s", "bucket": u("bucket"), "endpoint": "https://s3.example.test", "is_usable": True}],
        "cloud_provider_tokens": [], "integration_tokens": [], "dns_zones": [], "managed_dns_records": [],
        "ssl_certificates": [{"ssl_certificate": "CERT", "ssl_private_key": "PK", "configuration_dir": "/etc/x", "mount_path": "/m", "common_name": "c", "subject_alternative_names": ["a.example.test"], "valid_until": "2030-01-01T00:00:00+00:00", "is_ca_certificate": False, "resource_kind": "application", "resource_uuid": u("app")}],
        "volume_backups": [{"uuid": u("vb"), "backupable_kind": "persistent_volume", "backupable_uuid": u("vol"), "frequency": "daily", "enabled": True, "save_s3": True, "disable_local_backup": False, "stop_during_backup": False, "retention_amount_locally": 3, "retention_days_locally": 0, "retention_max_storage_locally": 0, "retention_amount_s3": 0, "retention_days_s3": 0, "retention_max_storage_s3": 0, "timeout": 3600, "missing_backup_notification_days": 0, "s3_storage_uuid": u("s3"), "s3_storage_uuids": [u("s3")], "had_s3_storage": True}],
        "server": {
            "uuid": u("srv"), "name": f"transfer {uuid}", "description": "imported", "ip": ip, "port": port, "user": "root",
            "proxy": {"type": "NONE", "status": "exited", "redirect_enabled": False},
            "server_role": "both", "is_build_server": False, "cloud_provider_token_uuid": None,
            "hetzner_server_id": None, "vultr_instance_id": None, "digitalocean_droplet_id": None, "hostinger_virtual_machine_id": None,
            "proxy_error_page": None, "swarm_cluster": None,
            "settings": {"concurrent_builds": 3, "wildcard_domain": "https://x.example.test", "docker_cleanup_frequency": "daily", "server_timezone": "UTC", "is_reachable": True, "force_disabled": True, "cloudflare_tunnel_integration_token_uuid": None},
        },
        "destinations": [{"uuid": u("dst"), "name": "net one", "network": u("net"), "type": "standalone"}],
        "github_runner_config": None,
        "shared_environment_variables": {
            "server": [{"key": "SRV_VAR", "value": "sv", "is_multiline": False, "is_literal": True, "is_shown_once": False, "comment": "c"}],
            "team": [{"key": f"TEAM_{uuid.upper()}", "value": "tv", "is_multiline": False, "is_literal": False, "is_shown_once": False, "comment": None}],
        },
        "projects": [{
            "uuid": u("prj"), "name": f"project {uuid}", "description": "pd", "icon": None,
            "shared_environment_variables": [{"key": "PROJ_VAR", "value": "pv"}],
            "environments": [{
                "uuid": u("env"), "name": "staging", "description": "ed", "canvas_positions": {u("app"): {"x": 1, "y": 2}, "other": {"x": 3}},
                "shared_environment_variables": [{"key": "ENV_VAR", "value": "ev"}],
                "applications": [{
                    "type": "application", "uuid": u("app"), "destination_uuid": u("dst"),
                    "attributes": {
                        "name": "app one", "description": "x", "fqdn": f"https://{uuid}.example.test", "git_repository": "coollabsio/coolify", "git_branch": "main",
                        "build_pack": "nixpacks", "ports_exposes": "3000", "ports_mappings": "", "base_directory": "src", "publish_directory": "dist", "status": "running:healthy",
                        "health_check_enabled": False, "custom_nginx_configuration": "server {}", "docker_compose_domains": None, "compose_parsing_version": "3",
                        "http_basic_auth_password": "hp", "manual_webhook_secret_github": "mg", "manual_webhook_secret_gitlab": "ml", "manual_webhook_secret_bitbucket": "mb", "manual_webhook_secret_gitea": "mt",
                        "noindex_domains": [], "domain_port_overrides": None, "is_maintenance_enabled": False, "id": 999, "unknown_column": "x", "restart_count": 2,
                    },
                    "settings": {"is_static": False, "is_auto_deploy_enabled": False, "is_force_https_enabled": False, "docker_images_to_keep": 4, "id": 1, "application_id": 2},
                    "environment_variables": [{"uuid": u("ev1"), "key": "A_KEY", "value": " spaced ", "is_literal": True, "is_multiline": False, "is_preview": False, "is_runtime": True, "is_buildtime": False, "is_shown_once": False, "is_shared": False, "uses_legacy_escaping": False, "preserve_compose_null": False, "comment": "cm", "order": 2, "version": "4.0.0"},
                                              {"uuid": u("ev2"), "key": "my-old-key", "value": None}],
                    "environment_variables_preview": [{"uuid": u("ev3"), "key": "A_KEY", "value": "pv"}],
                    "persistent_storages": [{"uuid": u("vol"), "name": f"vol-{uuid}", "mount_path": "data", "host_path": None, "is_preview_suffix_enabled": True}],
                    "file_storages": [{"uuid": u("fil"), "fs_path": "/data/x", "mount_path": "/x", "content": "hello", "is_directory": False, "is_host_file": False, "chown": "1000:1000", "chmod": "644", "is_based_on_git": False, "is_preview_suffix_enabled": False}],
                    "scheduled_tasks": [{"uuid": u("tsk"), "name": "t", "command": "ls", "frequency": "daily", "container": "web", "timeout": 60, "enabled": False}],
                    "tags": [{"uuid": u("tag"), "name": f" TAG{uuid} "}],
                    "previews": [{"uuid": u("prv"), "pull_request_id": 7, "pull_request_html_url": "https://github.com/x/y/pull/7", "fqdn": f"https://7.{uuid}.example.test", "status": "exited", "git_type": "github", "docker_compose_domains": None, "docker_registry_image_tag": None, "domain_port_overrides": None, "max_restart_count": 2, "persistent_storages": [{"uuid": u("pvl"), "name": f"pvol-{uuid}", "mount_path": "/p"}]}],
                    "secret_manager_link": {"uuid": u("sml"), "integration_token_uuid": "absent", "settings": {}},
                    "source": {"type": "github_app", "uuid": u("gh"), "system_wide": False},
                    "private_key_uuid": u("key"), "had_git_source": True, "had_private_key": True,
                }],
                "databases": [{
                    "type": "StandalonePostgresql", "uuid": u("dbp"), "destination_uuid": u("dst"),
                    "attributes": {"name": "pg", "postgres_user": "u", "postgres_password": "pw", "postgres_db": "d", "image": "postgres:16-alpine", "status": "running (healthy)", "is_public": False, "init_scripts": [], "ports_mappings": "", "id": 5},
                    "environment_variables": [{"uuid": u("ev4"), "key": "DB_VAR", "value": "dv"}],
                    "persistent_storages": [{"uuid": u("dvl"), "name": f"postgres-data-{uuid}", "mount_path": "/var/lib/postgresql/data"}],
                    "file_storages": [],
                    "tags": [],
                    "scheduled_backups": [{"uuid": u("bak"), "description": "b", "enabled": True, "save_s3": True, "frequency": "daily", "databases_to_backup": "d", "dump_all": False, "database_backup_retention_amount_locally": 3, "database_backup_retention_days_locally": 0, "database_backup_retention_max_storage_locally": 0, "database_backup_retention_amount_s3": 0, "database_backup_retention_days_s3": 0, "database_backup_retention_max_storage_s3": 0, "timeout": 3600, "s3_storage_uuid": u("s3"), "s3_storage_uuids": [u("s3")], "had_s3_storage": True}],
                    "secret_manager_link": None,
                }],
                "services": [{
                    "type": "service", "uuid": u("svc"), "destination_uuid": u("dst"),
                    "attributes": {"name": "svc", "docker_compose_raw": "services: {}", "docker_compose": "services: {}", "compose_parsing_version": "4", "include_coolify_default_variables": False, "service_type": None},
                    "environment_variables": [{"uuid": u("ev5"), "key": "SVC_VAR", "value": "x"}],
                    "scheduled_tasks": [], "tags": [{"name": f"tag{uuid}"}], "secret_manager_link": None,
                    "applications": [{"uuid": u("sap"), "attributes": {"name": "web", "image": "nginx", "status": "running:healthy", "fqdn": None, "exclude_from_status": False},
                                      "environment_variables": [], "persistent_storages": [{"uuid": u("svl"), "name": f"svol-{uuid}", "mount_path": "/s"}], "file_storages": []}],
                    "databases": [{"uuid": u("sdb"), "name": "thin-db", "status": None, "image": "postgres", "persistent_storages": [], "file_storages": [], "scheduled_backups": []}],
                }],
                "resource_links": [{"uuid": u("lnk"), "label": "  uses  ", "source": {"kind": "application", "uuid": u("app")}, "target": {"kind": "database", "uuid": u("dbp")}},
                                   {"uuid": u("lnx"), "label": None, "source": {"kind": "application", "uuid": u("app")}, "target": {"kind": "service", "uuid": "missing"}}],
            }],
        }],
    }
    b.update(over)
    return b


TABLES = [
    ("servers", "id", "SELECT id FROM servers WHERE uuid = '{uuid}srv'"),
]


def rows_for(uuid):
    """Every row the import of this bundle writes, normalized by uuid-independent content."""
    q = f"""
WITH s AS (SELECT * FROM servers WHERE uuid LIKE '{uuid}%'),
 prj AS (SELECT * FROM projects WHERE uuid LIKE '{uuid}%'),
 env AS (SELECT * FROM environments WHERE project_id IN (SELECT id FROM prj)),
 app AS (SELECT * FROM applications WHERE uuid LIKE '{uuid}%'),
 dbp AS (SELECT * FROM standalone_postgresqls WHERE uuid LIKE '{uuid}%'),
 svc AS (SELECT * FROM services WHERE uuid LIKE '{uuid}%')
SELECT json_build_object(
 'servers', (SELECT json_agg(row_to_json(x)) FROM s x),
 'settings', (SELECT json_agg(row_to_json(x)) FROM server_settings x WHERE server_id IN (SELECT id FROM s)),
 'dockers', (SELECT json_agg(row_to_json(x) ORDER BY x.id) FROM standalone_dockers x WHERE server_id IN (SELECT id FROM s)),
 'shared', (SELECT json_agg(row_to_json(x) ORDER BY x.key) FROM shared_environment_variables x WHERE server_id IN (SELECT id FROM s) OR project_id IN (SELECT id FROM prj) OR environment_id IN (SELECT id FROM env) OR key = 'TEAM_{uuid.upper()}'),
 'projects', (SELECT json_agg(row_to_json(x)) FROM prj x),
 'project_settings', (SELECT json_agg(row_to_json(x)) FROM project_settings x WHERE project_id IN (SELECT id FROM prj)),
 'envs', (SELECT json_agg(row_to_json(x) ORDER BY x.name) FROM env x),
 'apps', (SELECT json_agg(row_to_json(x)) FROM app x),
 'app_settings', (SELECT json_agg(row_to_json(x)) FROM application_settings x WHERE application_id IN (SELECT id FROM app)),
 'previews', (SELECT json_agg(row_to_json(x)) FROM application_previews x WHERE application_id IN (SELECT id FROM app)),
 'dbs', (SELECT json_agg(row_to_json(x)) FROM dbp x),
 'services', (SELECT json_agg(row_to_json(x)) FROM svc x),
 'service_apps', (SELECT json_agg(row_to_json(x)) FROM service_applications x WHERE service_id IN (SELECT id FROM svc)),
 'service_dbs', (SELECT json_agg(row_to_json(x)) FROM service_databases x WHERE service_id IN (SELECT id FROM svc)),
 'envvars', (SELECT json_agg(row_to_json(x) ORDER BY x.uuid) FROM environment_variables x WHERE uuid LIKE '{uuid}%'),
 'volumes', (SELECT json_agg(row_to_json(x) ORDER BY x.uuid) FROM local_persistent_volumes x WHERE uuid LIKE '{uuid}%'),
 'files', (SELECT json_agg(row_to_json(x)) FROM local_file_volumes x WHERE uuid LIKE '{uuid}%'),
 'tasks', (SELECT json_agg(row_to_json(x)) FROM scheduled_tasks x WHERE uuid LIKE '{uuid}%'),
 'tags', (SELECT json_agg(row_to_json(x)) FROM tags x WHERE name LIKE 'tag{uuid}%'),
 'taggables', (SELECT count(*) FROM taggables WHERE tag_id IN (SELECT id FROM tags WHERE name LIKE 'tag{uuid}%')),
 'db_backups', (SELECT json_agg(row_to_json(x)) FROM scheduled_database_backups x WHERE uuid LIKE '{uuid}%'),
 'vol_backups', (SELECT json_agg(row_to_json(x)) FROM scheduled_volume_backups x WHERE uuid LIKE '{uuid}%'),
 's3', (SELECT json_agg(row_to_json(x)) FROM s3_storages x WHERE uuid LIKE '{uuid}%'),
 's3_link_db', (SELECT count(*) FROM scheduled_database_backup_s3_storage WHERE scheduled_database_backup_id IN (SELECT id FROM scheduled_database_backups WHERE uuid LIKE '{uuid}%')),
 's3_link_vol', (SELECT count(*) FROM scheduled_volume_backup_s3_storage WHERE scheduled_volume_backup_id IN (SELECT id FROM scheduled_volume_backups WHERE uuid LIKE '{uuid}%')),
 'github', (SELECT json_agg(row_to_json(x)) FROM github_apps x WHERE uuid LIKE '{uuid}%'),
 'keys', (SELECT json_agg(row_to_json(x)) FROM private_keys x WHERE uuid LIKE '{uuid}%'),
 'links', (SELECT json_agg(row_to_json(x)) FROM resource_links x WHERE uuid LIKE '{uuid}%'),
 'certs', (SELECT json_agg(row_to_json(x)) FROM ssl_certificates x WHERE server_id IN (SELECT id FROM s))
)::text"""
    return json.loads(sql(q))


ENCRYPTED = {"private_key", "client_secret", "webhook_secret", "value", "key", "secret", "fs_path", "mount_path", "content",
             "postgres_password", "http_basic_auth_password", "manual_webhook_secret_github", "manual_webhook_secret_gitlab",
             "manual_webhook_secret_bitbucket", "manual_webhook_secret_gitea", "ssl_certificate", "ssl_private_key", "sentinel_token"}
VOLATILE = re.compile(r"(^id$|_id$|created_at|updated_at|last_online_at|server_metadata|sentinel_custom_url|^uuid$)")


def clean(rows, uuid):
    """Ids and times differ; encrypted columns are compared by presence."""

    def walk(v, table=None, key=None):
        if isinstance(v, dict):
            out = {}
            for k, x in v.items():
                keep_id = k in ("id",)
                if VOLATILE.search(k) and k not in ("uuid", "pull_request_id", "app_id", "installation_id", "client_id"):
                    out[k] = "<present>" if x is not None else None
                    continue
                if k in ENCRYPTED and isinstance(x, str) and x.startswith("eyJ"):
                    out[k] = "<encrypted>"
                    continue
                if k == "uuid" and isinstance(x, str) and not x.startswith(uuid):
                    out[k] = "<generated>"
                    continue
                out[k] = walk(x)
                del keep_id
            return out
        if isinstance(v, list):
            return [walk(x) for x in v]
        return v

    return walk(rows)


def delete_import(uuid, ip=None):
    """Deletes exactly what an import of this bundle created. The bundle's names and uuid prefix are
    unique to this script; without preserved uuids the rows are found from the server and project."""
    stmts = f"""
DO $$
DECLARE r record;
BEGIN
 CREATE TEMP TABLE tsrv ON COMMIT DROP AS SELECT id FROM servers WHERE uuid LIKE '{uuid}%' OR ip = '{ip or uuid}';
 CREATE TEMP TABLE tprj ON COMMIT DROP AS SELECT id FROM projects WHERE uuid LIKE '{uuid}%' OR name = 'project {uuid}';
 CREATE TEMP TABLE tenv ON COMMIT DROP AS SELECT id FROM environments WHERE project_id IN (SELECT id FROM tprj);
 CREATE TEMP TABLE tapp ON COMMIT DROP AS SELECT id FROM applications WHERE uuid LIKE '{uuid}%' OR environment_id IN (SELECT id FROM tenv);
 CREATE TEMP TABLE tdb ON COMMIT DROP AS SELECT id FROM standalone_postgresqls WHERE uuid LIKE '{uuid}%' OR environment_id IN (SELECT id FROM tenv);
 CREATE TEMP TABLE tsvc ON COMMIT DROP AS SELECT id FROM services WHERE uuid LIKE '{uuid}%' OR environment_id IN (SELECT id FROM tenv);
 DELETE FROM scheduled_volume_backup_s3_storage WHERE scheduled_volume_backup_id IN (SELECT id FROM scheduled_volume_backups WHERE uuid LIKE '{uuid}%');
 DELETE FROM scheduled_database_backup_s3_storage WHERE scheduled_database_backup_id IN (SELECT id FROM scheduled_database_backups WHERE uuid LIKE '{uuid}%');
 DELETE FROM scheduled_volume_backups WHERE uuid LIKE '{uuid}%';
 DELETE FROM scheduled_database_backups WHERE uuid LIKE '{uuid}%';
 DELETE FROM scheduled_tasks WHERE uuid LIKE '{uuid}%';
 DELETE FROM environment_variables WHERE uuid LIKE '{uuid}%';
 DELETE FROM local_persistent_volumes WHERE uuid LIKE '{uuid}%';
 DELETE FROM local_file_volumes WHERE uuid LIKE '{uuid}%';
 DELETE FROM resource_links WHERE environment_id IN (SELECT id FROM tenv);
 DELETE FROM taggables WHERE tag_id IN (SELECT id FROM tags WHERE name LIKE 'tag{uuid}%');
 DELETE FROM tags WHERE name LIKE 'tag{uuid}%';
 DELETE FROM application_previews WHERE application_id IN (SELECT id FROM tapp);
 DELETE FROM application_settings WHERE application_id IN (SELECT id FROM tapp);
 DELETE FROM applications WHERE id IN (SELECT id FROM tapp);
 DELETE FROM standalone_postgresqls WHERE id IN (SELECT id FROM tdb);
 DELETE FROM service_applications WHERE service_id IN (SELECT id FROM tsvc);
 DELETE FROM service_databases WHERE service_id IN (SELECT id FROM tsvc);
 DELETE FROM services WHERE id IN (SELECT id FROM tsvc);
 DELETE FROM shared_environment_variables WHERE key = 'TEAM_{uuid.upper()}' OR server_id IN (SELECT id FROM tsrv) OR project_id IN (SELECT id FROM tprj) OR environment_id IN (SELECT id FROM tenv);
 DELETE FROM project_settings WHERE project_id IN (SELECT id FROM tprj);
 DELETE FROM environments WHERE id IN (SELECT id FROM tenv);
 DELETE FROM projects WHERE id IN (SELECT id FROM tprj);
 DELETE FROM server_settings WHERE server_id IN (SELECT id FROM tsrv);
 DELETE FROM ssl_certificates WHERE server_id IN (SELECT id FROM tsrv);
 DELETE FROM standalone_dockers WHERE server_id IN (SELECT id FROM tsrv);
 DELETE FROM swarm_dockers WHERE server_id IN (SELECT id FROM tsrv);
 DELETE FROM servers WHERE id IN (SELECT id FROM tsrv);
 DELETE FROM github_apps WHERE uuid LIKE '{uuid}%' OR name = '{uuid} gh app';
 DELETE FROM s3_storages WHERE uuid LIKE '{uuid}%' OR bucket = '{uuid}bucket';
 DELETE FROM private_keys WHERE uuid LIKE '{uuid}%' OR name = '{uuid} Transfer key';
END $$;"""
    sql(stmts)


def errors():
    same("check missing fields", "GET", "/api/v1/servers/import/check")
    same("check bad ip and port", "GET", "/api/v1/servers/import/check?server_uuid=x&ip=not%20valid&port=99999&schema_version=abc")
    same("check unreachable", "GET", f"/api/v1/servers/import/check?server_uuid=freshuuid&ip={UNREACHABLE_IP}&port=1")
    same("check old schema", "GET", f"/api/v1/servers/import/check?server_uuid=freshuuid&ip={UNREACHABLE_IP}&port=1&schema_version=9")
    same("check missing sources", "GET", f"/api/v1/servers/import/check?server_uuid=x&ip={UNREACHABLE_IP}&port=1&system_github_apps[]=nope&system_gitlab_apps[]=nope2")
    same("check existing uuid", "GET", f"/api/v1/servers/import/check?server_uuid=hml20fsj5eles0xfiqidq8j5&ip={UNREACHABLE_IP}&port=1")
    same("check member forbidden", "GET", "/api/v1/servers/import/check", tok="acme_member_write_legacy")
    same("check read token", "GET", "/api/v1/servers/import/check", tok="acme_owner_read")
    same("check write token no deploy", "GET", "/api/v1/servers/import/check", tok="acme_owner_write")

    same("import empty", "POST", "/api/v1/servers/import", {"dry_run": True})
    same("import extra", "POST", "/api/v1/servers/import", {"bundle": {"a": 1}, "nope": 1})
    same("import bundle string", "POST", "/api/v1/servers/import", {"bundle": "x"})
    same("import bool errors", "POST", "/api/v1/servers/import", {"bundle": {"a": 1}, "dry_run": "maybe", "adopt_mode": 5})
    same("import invalid bundle", "POST", "/api/v1/servers/import", {"bundle": {"a": 1}})
    good = bundle("tdiffa")
    same("import dry run", "POST", "/api/v1/servers/import", {"bundle": good, "dry_run": True})
    same("import dry run no uuids", "POST", "/api/v1/servers/import", {"bundle": good, "dry_run": True, "preserve_uuids": False, "adopt_mode": False})
    same("import adopt needs uuids", "POST", "/api/v1/servers/import", {"bundle": good, "dry_run": True, "preserve_uuids": False})
    same("import wrong schema", "POST", "/api/v1/servers/import", {"bundle": {**good, "schema_version": 3}, "dry_run": True})
    same("import newer version", "POST", "/api/v1/servers/import", {"bundle": {**good, "coolify_version": "99.0.0"}, "dry_run": True})
    bad = json.loads(json.dumps(good))
    bad["server"]["uuid"] = "bad id!"
    same("import bad id", "POST", "/api/v1/servers/import", {"bundle": bad, "dry_run": True})
    bad = json.loads(json.dumps(good))
    bad["destinations"][0]["network"] = "bad net"
    bad["github_apps"][0]["html_url"] = "ftp://x.example.test"
    bad["ssl_certificates"][0]["mount_path"] = "/x;rm"
    bad["volume_backups"][0]["frequency"] = "nope"
    e = bad["projects"][0]["environments"][0]
    e["applications"][0]["attributes"]["fqdn"] = "ftp://bad.example.test,https://nodots,https://ok.example.test:99999"
    e["applications"][0]["scheduled_tasks"][0]["command"] = ""
    e["applications"][0]["scheduled_tasks"][0]["container"] = "bad name"
    e["applications"][0]["scheduled_tasks"][0]["frequency"] = "bad"
    e["databases"][0]["scheduled_backups"][0]["frequency"] = "nope"
    e["services"][0]["applications"][0]["persistent_storages"][0]["mount_path"] = "/a|b"
    same("import field checks", "POST", "/api/v1/servers/import", {"bundle": bad, "dry_run": True})
    bad = json.loads(json.dumps(good))
    bad["server"]["ip"] = "not valid"
    bad["server"]["port"] = "x"
    bad["server"]["user"] = "a b"
    same("import server payload", "POST", "/api/v1/servers/import", {"bundle": bad, "dry_run": True})
    same("import too many", "POST", "/api/v1/servers/import", {"bundle": {**good, "destinations": [{"uuid": "a"}] * 1001}, "dry_run": True})
    same("import existing uuid and ip", "POST", "/api/v1/servers/import", {"bundle": {**good, "server": {**good["server"], "uuid": "hml20fsj5eles0xfiqidq8j5"}}})
    same("import member forbidden", "POST", "/api/v1/servers/import", {"bundle": good}, tok="acme_member_write_legacy")
    same("import read token", "POST", "/api/v1/servers/import", {"bundle": good}, tok="acme_owner_read")

    missing = "/api/v1/servers/nope"
    same("status 404", "GET", f"{missing}/transfer")
    same("status existing", "GET", "/api/v1/servers/hml20fsj5eles0xfiqidq8j5/transfer")
    same("status member", "GET", "/api/v1/servers/hml20fsj5eles0xfiqidq8j5/transfer", tok="acme_member_read")
    same("status other team", "GET", "/api/v1/servers/hml20fsj5eles0xfiqidq8j5/transfer", tok="carol_root")
    same("complete 404", "POST", f"{missing}/transfer/complete", {})
    same("complete member", "POST", "/api/v1/servers/hml20fsj5eles0xfiqidq8j5/transfer/complete", {}, tok="acme_member_write_legacy")
    same("complete bad url", "POST", "/api/v1/servers/hml20fsj5eles0xfiqidq8j5/transfer/complete", {"target_instance_url": "http://x.example.test"})
    same("complete bad types", "POST", "/api/v1/servers/hml20fsj5eles0xfiqidq8j5/transfer/complete", {"export_id": 5, "target_instance_url": 5})
    same("migrate 404", "POST", f"{missing}/migrate", {"target_url": "https://x.example.test", "target_token": "t"})
    same("migrate no sensitive", "POST", "/api/v1/servers/hml20fsj5eles0xfiqidq8j5/migrate", {}, tok="acme_owner_write")
    same("migrate empty", "POST", "/api/v1/servers/hml20fsj5eles0xfiqidq8j5/migrate", {"a": 1})
    same("migrate bad url", "POST", "/api/v1/servers/hml20fsj5eles0xfiqidq8j5/migrate", {"target_url": "nope", "target_token": "t"})
    same("migrate http", "POST", "/api/v1/servers/hml20fsj5eles0xfiqidq8j5/migrate", {"target_url": "http://x.example.test", "target_token": "t"})
    same("migrate private", "POST", "/api/v1/servers/hml20fsj5eles0xfiqidq8j5/migrate", {"target_url": "https://127.0.0.1", "target_token": "t"})
    same("migrate blank token", "POST", "/api/v1/servers/hml20fsj5eles0xfiqidq8j5/migrate", {"target_url": "https://1.1.1.1.invalid", "target_token": "   "})


def imports():
    """A real import on each side, with an address that refuses, so the claim fails the same way."""
    for label, over, preserve in [("adopt", {}, True), ("no adopt", {}, False)]:
        results = {}
        for side in ("ref", "rs"):
            uuid = "tdiff" + side[0] + ("p" if preserve else "n")
            payload = {"bundle": bundle(uuid, ip=UNREACHABLE_IP + ("1" if preserve else "2"), **over)}
            if not preserve:
                payload.update(preserve_uuids=False, adopt_mode=False)
            ip = UNREACHABLE_IP + ("1" if preserve else "2")
            delete_import(uuid, ip)
            try:
                status, body = call(side, "POST", "/api/v1/servers/import", payload)
                results[side] = (status, body, clean(rows_for(uuid), uuid))
            finally:
                delete_import(uuid, ip)
        a, b = results["ref"], results["rs"]

        def scrub(v):
            text = json.dumps(norm(v), sort_keys=True)
            text = re.sub(r"tdiff[rs][pn]", "UUID", text)
            text = re.sub(r"TDIFF[RS][PN]", "UUID", text)
            text = re.sub(r"automatic claim failed: [^\"]*", "automatic claim failed", text)
            text = re.sub(r'"(server_uuid|private_key_uuid)": "[a-z0-9]{20,}"', '"server_uuid": "G"', text)
            return json.loads(text)

        compare(f"import response {label}", (a[0], scrub(a[1])), (b[0], scrub(b[1])))
        ra, rb = scrub(a[2]), scrub(b[2])
        for key in sorted(ra):
            compare(f"import rows {label}: {key}", ra[key], rb.get(key))


HOST_ADDRESS = {"ref": ("th-root", 22), "rs": ("127.0.0.1", 2223)}


def tinker(php):
    out = subprocess.run(
        ["docker", "exec", "coolify-rs-reference-coolify-1", "php", "/var/www/html/artisan", "tinker", "--execute", php],
        check=True, capture_output=True, text=True,
    ).stdout
    return out.strip().splitlines()[-1] if out.strip() else ""


def marker():
    r = subprocess.run(["docker", "exec", "coolify-rs-testing-host", "cat", "/data/coolify/transfer/owner.json"], capture_output=True, text=True)
    return json.loads(r.stdout) if r.returncode == 0 else None


def clear_marker():
    subprocess.run(["docker", "exec", "coolify-rs-testing-host", "rm", "-f", "/data/coolify/transfer/owner.json"], capture_output=True)


def scrub_ids(v, uuid):
    text = json.dumps(norm(v), sort_keys=True).replace(uuid, "UUID").replace(uuid.upper(), "UUID")
    for address in ("th-root", "127.0.0.1"):
        text = text.replace(address, "HOST")
    return json.loads(text)


def metadata_of(uuid):
    """Only the transfer part: the validate job also adds the hardware of the host."""
    meta = json.loads(sql(f"SELECT server_metadata::text FROM servers WHERE uuid = '{uuid}srv'") or "null") or {}
    return {k: v for k, v in meta.items() if k in ("transfer", "migration")}


def settings_of(uuid):
    return json.loads(sql(f"SELECT row_to_json(s)::text FROM server_settings s JOIN servers v ON v.id = s.server_id WHERE v.uuid = '{uuid}srv'") or "null")


def claims():
    """A real import and claim over real ssh: the reference reaches the testing host on its network,
    coolify-rs on 127.0.0.1:2223 (start it with COOLIFY_RS_SSH_BINARY=/usr/bin/ssh)."""
    results = {}
    for side in ("ref", "rs"):
        uuid = "tclm" + side
        ip, port = HOST_ADDRESS[side]
        delete_import(uuid, ip if ip != "127.0.0.1" else None)
        clear_marker()
        try:
            status, body = call(side, "POST", "/api/v1/servers/import", {"bundle": bundle(uuid, ip=ip, port=port)})
            time.sleep(15)
            results[side] = dict(
                status=status,
                body=scrub_ids(body, uuid),
                metadata=scrub_ids(metadata_of(uuid), uuid),
                settings=clean({k: v for k, v in settings_of(uuid).items()}, uuid) if settings_of(uuid) else None,
                marker=sorted((marker() or {}).keys()),
                marker_url=(marker() or {}).get("instance_url"),
                status_api=scrub_ids(call(side, "GET", f"/api/v1/servers/{uuid}srv/transfer")[1], uuid),
            )
            # Mark it transferred through the API on the same side: `complete`.
            first = call(side, "POST", f"/api/v1/servers/{uuid}srv/transfer/complete", {"export_id": "e1", "target_instance_url": "https://target.example.test/"})
            results[side]["complete"] = (first[0], scrub_ids(first[1], uuid))
            results[side]["after"] = scrub_ids(metadata_of(uuid), uuid)
            results[side]["after_settings"] = clean(settings_of(uuid), uuid) if settings_of(uuid) else None
        finally:
            delete_import(uuid, ip if ip != "127.0.0.1" else None)
            clear_marker()
    a, b = results["ref"], results["rs"]
    for key in sorted(a):
        va, vb = a[key], b[key]
        if key in ("settings", "after_settings"):
            for volatile in ("sentinel_custom_url", "docker_version_checked_at", "compose_version_checked_at"):
                if isinstance(va, dict):
                    va = {k: x for k, x in va.items() if k != volatile}
                    vb = {k: x for k, x in vb.items() if k != volatile}
        if key == "marker_url":
            continue
        compare(f"claim {key}", json.loads(json.dumps(va).replace("http://127.0.0.1:18000", "URL").replace("http://localhost", "URL")), json.loads(json.dumps(vb).replace("http://127.0.0.1:18000", "URL").replace("http://localhost", "URL")))


class Fake:
    """A recorded fake of the target instance for `migrate`: nothing leaves this machine."""

    def __init__(self):
        import http.server
        import threading

        fake = self
        self.requests = []
        self.bundle = None

        class H(http.server.BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def send(self, code, value):
                data = json.dumps(value).encode()
                self.send_response(code)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def do_GET(self):
                fake.requests.append(("GET", self.path, self.headers.get("Authorization")))
                self.send(200, {"message": "ok", "checks": [{"label": "fake check", "ok": True, "message": None}], "missing_system_sources": {"github": [], "gitlab": []}})

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                fake.requests.append(("POST", self.path, self.headers.get("Authorization")))
                fake.bundle = body.get("bundle")
                self.send(201, {"claimed": True, "warnings": ["target warning"], "checklist": ["target step"], "server_url": "https://target.example.test/server/x"})

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
        self.port = self.server.server_address[1]
        threading.Thread(target=self.server.serve_forever, daemon=True).start()


def canonical(v):
    """Lists whose order comes from the heap sort by uuid; bundle-level volatile values go."""
    if isinstance(v, dict):
        return {k: canonical(x) for k, x in v.items() if k not in ("exported_at", "export_id", "coolify_version", "source_instance", "logdrain_custom_config_parser")}
    if isinstance(v, list):
        items = [canonical(x) for x in v]
        return sorted(items, key=lambda x: json.dumps(x, sort_keys=True))
    return v


def export():
    """Import on coolify-rs, compare the exports of that server, then run the job against the fake."""
    uuid = "texp"
    ip, port = HOST_ADDRESS["rs"]
    delete_import(uuid)
    clear_marker()
    fake = Fake()
    try:
        status, body = call("rs", "POST", "/api/v1/servers/import", {"bundle": bundle(uuid, ip=ip, port=port)})
        record("import for export", status == 201 and body.get("claimed") is True, str(body)[:300])
        time.sleep(2)
        php = f'echo json_encode(app(\\App\\Services\\ServerTransfer\\ServerTransferExporter::class)->export(\\App\\Models\\Server::where("uuid","{uuid}srv")->first()));'
        reference = json.loads(tinker(php.replace("\\\\", "\\")))

        target = f"http://127.0.0.1:{fake.port}"
        code, started = call("rs", "POST", f"/api/v1/servers/{uuid}srv/migrate", {"target_url": target, "target_token": "Bearer faketoken"})
        record("migrate started", code == 202, str(started)[:600])
        status_body = {}
        for _ in range(30):
            time.sleep(1)
            status_body = call("rs", "GET", f"/api/v1/servers/{uuid}srv/transfer")[1]
            if (status_body.get("migration") or {}).get("status") != "running":
                break
        record("migrate succeeded", (status_body.get("migration") or {}).get("status") == "succeeded", json.dumps(status_body)[:1500])
        record("fake saw check then import", [r[0] for r in fake.requests] == ["GET", "POST"] and all(r[2] == "Bearer faketoken" for r in fake.requests), str(fake.requests))
        record("transfer status transferred", (status_body.get("transfer") or {}).get("status") == "transferred", json.dumps(status_body)[:400])
        record("migration warnings", "target warning" in (status_body.get("migration") or {}).get("warnings", []), "")
        ra, rb = canonical(reference), canonical(fake.bundle or {})
        for key in sorted(set(ra) | set(rb)):
            compare(f"bundle {key}", ra.get(key), rb.get(key))
        for line in (status_body.get("migration") or {}).get("log", []):
            print("  log:", line["level"], line["message"][:140])
    finally:
        delete_import(uuid)
        clear_marker()
        fake.server.shutdown()


def main():
    sections = sys.argv[1:] or ["errors", "imports"]
    for s in sections:
        globals()[s]()


if __name__ == "__main__":
    main()
