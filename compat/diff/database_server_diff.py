#!/usr/bin/env python3
"""Real server work of databases on the test VM, through both implementations.

    COOLIFY_RS_BASE=http://127.0.0.1:18083 compat/diff/database_server_diff.py --s3 http://<ip>:9000 [--kinds postgresql,...] [--parts ...]

For each engine one database per side: public TCP proxy (files, container, a real TCP connection),
SSL (files, certificates, a TLS handshake, rows), init scripts, custom Docker options, file storages,
scheduled backup, S3 upload, restore from server/S3/upload. Compose files and proxy files are
compared byte for byte with the UUIDs and secrets normalized. Everything created is removed.
"""
import argparse
import base64
import difflib
import json
import os
import re
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import databases_diff as dd  # noqa: E402

call, sql, record = dd.call, dd.sql, dd.record
SERVER = "hml20fsj5eles0xfiqidq8j5"
VM = "coolify-rs-e2e"
ROOT = "acme_owner_root"
SIDES = ("ref", "rs")
TABLES = {k: v["table"] for k, v in dd.KINDS.items()}
TABLES["sqlite"] = "standalone_sqlites"
MODELS = {k: v["model"] for k, v in dd.KINDS.items()}
MODELS["sqlite"] = "StandaloneSqlite"
FIXED = {k: v["fixed"] for k, v in dd.KINDS.items()}
FIXED["postgresql"] = {"postgres_password": "Same-Pass_2"}
FIXED["sqlite"] = {}
BACKUPS = {"postgresql", "mysql", "mariadb", "mongodb", "clickhouse", "sqlite"}
IMPORTS = {"postgresql", "mysql", "mariadb", "mongodb", "sqlite"}
SSL = {"postgresql", "mysql", "mariadb", "mongodb", "redis", "keydb", "dragonfly"}
PROXY = set(dd.KINDS)
# A write into each engine, a read back; {u} is the container.
SEED = {
    "postgresql": ("psql -U postgres -d postgres -c \"CREATE TABLE IF NOT EXISTS t(v text); DELETE FROM t; INSERT INTO t VALUES ('seed-1')\"",
                   "psql -U postgres -d postgres -Atc \"SELECT v FROM t\"",
                   "psql -U postgres -d postgres -c \"DELETE FROM t\""),
    "mysql": ("mysql -uroot -pSame-Root_3 default -e \"CREATE TABLE IF NOT EXISTS t(v text); DELETE FROM t; INSERT INTO t VALUES ('seed-1')\"",
              "mysql -uroot -pSame-Root_3 default -N -e \"SELECT v FROM t\"",
              "mysql -uroot -pSame-Root_3 default -e \"DELETE FROM t\""),
    "mariadb": ("mariadb -uroot -pSame-Root_3 default -e \"CREATE TABLE IF NOT EXISTS t(v text); DELETE FROM t; INSERT INTO t VALUES ('seed-1')\"",
                "mariadb -uroot -pSame-Root_3 default -N -e \"SELECT v FROM t\"",
                "mariadb -uroot -pSame-Root_3 default -e \"DELETE FROM t\""),
    "mongodb": ("mongosh --quiet -u root -p Same-Pass_2 --authenticationDatabase admin default --eval \"db.t.deleteMany({}); db.t.insertOne({v:'seed-1'})\"",
                "mongosh --quiet -u root -p Same-Pass_2 --authenticationDatabase admin default --eval \"db.t.findOne().v\"",
                "mongosh --quiet -u root -p Same-Pass_2 --authenticationDatabase admin default --eval \"db.t.deleteMany({})\""),
    "sqlite": ("sqlite3 /var/lib/sqlite/database.sqlite \"CREATE TABLE IF NOT EXISTS t(v text); DELETE FROM t; INSERT INTO t VALUES ('seed-1')\"",
               "sqlite3 /var/lib/sqlite/database.sqlite \"SELECT v FROM t\"",
               "sqlite3 /var/lib/sqlite/database.sqlite \"DELETE FROM t\""),
    "clickhouse": ("clickhouse-client --user default --password Same-Pass_2 --query \"CREATE TABLE IF NOT EXISTS default.t(v String) ENGINE=MergeTree ORDER BY v\" && clickhouse-client --user default --password Same-Pass_2 --query \"INSERT INTO default.t VALUES ('seed-1')\"",
                   "clickhouse-client --user default --password Same-Pass_2 --query \"SELECT v FROM default.t\"", ""),
}


def vm(cmd, timeout=300):
    r = subprocess.run(["orb", "-m", VM, "-u", "root", "bash", "-c", cmd], capture_output=True, text=True, timeout=timeout)
    return r.stdout + r.stderr


def vm_out(cmd):
    return subprocess.run(["orb", "-m", VM, "-u", "root", "bash", "-c", cmd], capture_output=True, text=True, timeout=300).stdout


def wait_status(table, u, want="running", tries=120):
    s = ""
    for _ in range(tries):
        s = sql(f"SELECT status FROM {table} WHERE uuid='{u}'")
        if s.startswith(want) and "starting" not in s:
            return s
        time.sleep(2)
    return s


def norm(text, uuids):
    for u in uuids:
        text = text.replace(u, "UUID")
    text = re.sub(r"Latest Deployment Date: .*", "Latest Deployment Date: D", text)
    # Upstream reads file storages and variables without an order (heap order): compare those
    # runs of lines sorted.
    def sortable(line):
        return line.startswith("            - '/data/coolify/databases/") or re.match(r" {12}- [A-Z_]+=", line)
    lines, out, run = text.split("\n"), [], []
    for line in lines:
        if sortable(line):
            run.append(line)
            continue
        out += sorted(run)
        run = []
        out.append(line)
    return "\n".join(out + sorted(run))


def same_file(name, path_fn, sides_uuid):
    a = norm(vm_out(f"cat {path_fn(sides_uuid['ref'])} 2>&1"), sides_uuid.values())
    b = norm(vm_out(f"cat {path_fn(sides_uuid['rs'])} 2>&1"), sides_uuid.values())
    record(name, a == b and a != "", "\n".join(difflib.unified_diff(a.splitlines(), b.splitlines(), "ref", "rs", lineterm=""))[:2500])


def cleanup_vm(uuid):
    vm(f"docker rm -f {uuid} {uuid}-proxy >/dev/null 2>&1; rm -rf /data/coolify/databases/{uuid}")


def activity_log(u):
    text = sql(f"SELECT description FROM activity_log WHERE properties->>'type_uuid'='{u}' AND properties->>'operation'='database-start' ORDER BY id DESC LIMIT 1")
    try:
        return [e.get("output", "") for e in json.loads(text)]
    except ValueError:
        return []


def run_kind(kind, a, project, s3):
    table = TABLES[kind]
    morph = "App\\Models\\" + MODELS[kind]
    endpoint = f"/api/v1/databases/{kind}"
    base = {"server_uuid": SERVER, "project_uuid": project, "environment_name": "production", "name": f"srv-{kind}", **FIXED[kind]}
    u = {}
    for side in SIDES:
        s, r = call(side, "POST", endpoint, base, ROOT)
        if s != 201:
            record(f"{kind}: create on {side}", False, r)
            return
        u[side] = json.loads(r)["uuid"]
    uuids = u
    try:
        # --- plain start with custom options, a file storage and (postgres) an init script
        opts = "--cap-add SYS_NICE --ulimit nofile=4096:8192 --shm-size 128m --hostname x" + kind
        for side in SIDES:
            sql_update = f"UPDATE {table} SET custom_docker_run_options='{opts}', " + \
                ("init_scripts='[{\"index\":0,\"filename\":\"a.sql\",\"content\":\"CREATE TABLE seeded(v text);\"}]'::json, " if kind == "postgresql" else "") + \
                f"updated_at=updated_at WHERE uuid='{u[side]}'"
            sql(sql_update)
            s, r = call(side, "POST", f"/api/v1/databases/{u[side]}/storages", {"type": "file", "mount_path": "/conf/app.txt", "content": "file-content"}, ROOT)
            if s != 201:
                record(f"{kind}: file storage on {side}", False, r)
            call(side, "PATCH", f"/api/v1/databases/{u[side]}/envs/bulk", {"data": [{"key": "CUSTOM_VAR", "value": "cv"}]}, ROOT)
            s, r = call(side, "POST", f"/api/v1/databases/{u[side]}/start", tok=ROOT)
            if s != 200:
                record(f"{kind}: start on {side}", False, r)
        st = {s: wait_status(table, u[s]) for s in SIDES}
        record(f"{kind}: started", all(v.startswith("running") for v in st.values()), st)
        same_file(f"{kind}: compose file (options, file mount, init script)", lambda x: f"/data/coolify/databases/{x}/docker-compose.yml", u)
        same_file(f"{kind}: stored file", lambda x: f"/data/coolify/databases/{x}/conf/app.txt", u)
        lines = {s: [x for l in activity_log(u[s]) for x in l.splitlines() if "ssl" in x.lower()] for s in SIDES}
        record(f"{kind}: start log lines", lines["ref"] == lines["rs"], lines)
        insp = "docker inspect --format '{{.HostConfig.CapAdd}} {{.HostConfig.Ulimits}} {{.HostConfig.ShmSize}} {{.Config.Hostname}}' "
        i = {s: vm_out(insp + u[s]) for s in SIDES}
        record(f"{kind}: container options", i["ref"] == i["rs"] and "SYS_NICE" in i["ref"], i)

        # --- logs
        status = {}
        for side in SIDES:
            s, r = call(side, "GET", f"/api/v1/databases/{u[side]}/logs?lines=5", tok=ROOT)
            status[side] = (s, sorted(json.loads(r).keys()) if s == 200 else r)
        record(f"{kind}: logs answer", status["ref"] == status["rs"], status)

        # --- TCP proxy
        if "proxy" in a.parts and kind in PROXY:
            ports = {"ref": a.port_base, "rs": a.port_base + 1}
            for side in SIDES:
                s, r = call(side, "PATCH", f"/api/v1/databases/{u[side]}", {"is_public": True, "public_port": ports[side]}, ROOT)
                record(f"{kind}: make public on {side}", s == 200, r)
            for side in SIDES:
                for _ in range(40):
                    if "running" in vm_out(f"docker inspect --format '{{{{.State.Status}}}}' {u[side]}-proxy 2>/dev/null"):
                        break
                    time.sleep(2)
            def proxy_norm(x, side):
                return norm(x, u.values()).replace(str(ports[side]), "PORT")
            for f in ("nginx.conf", "docker-compose.yaml"):
                a1 = proxy_norm(vm_out(f"cat /data/coolify/databases/{u['ref']}/proxy/{f}"), "ref")
                b1 = proxy_norm(vm_out(f"cat /data/coolify/databases/{u['rs']}/proxy/{f}"), "rs")
                record(f"{kind}: proxy {f}", a1 == b1 and a1 != "", "\n".join(difflib.unified_diff(a1.splitlines(), b1.splitlines(), lineterm=""))[:1500])
            conn = {s: vm_out(f"timeout 5 bash -c 'exec 3<>/dev/tcp/127.0.0.1/{ports[s]}' && echo open || echo closed").strip() for s in SIDES}
            record(f"{kind}: proxy accepts connections", conn == {"ref": "open", "rs": "open"}, conn)
            for side in SIDES:
                call(side, "PATCH", f"/api/v1/databases/{u[side]}", {"is_public": False}, ROOT)
            gone = {}
            for _ in range(20):
                gone = {s: vm_out(f"docker ps -a --filter name={u[s]}-proxy --format '{{{{.Names}}}}'").strip() for s in SIDES}
                if gone == {"ref": "", "rs": ""}:
                    break
                time.sleep(3)
            record(f"{kind}: proxy removed", gone == {"ref": "", "rs": ""}, gone)

        # --- backup, S3, restore
        if "backup" in a.parts and kind in BACKUPS:
            run_backup(kind, u, table, s3)

        # --- clone with its volume data
        if "clone" in a.parts and kind in SEED:
            run_clone(kind, u, table)

        # --- SSL
        if "ssl" in a.parts and kind in SSL:
            run_ssl(kind, u, table, morph)
    finally:
        for side in SIDES:
            call(side, "DELETE", f"/api/v1/databases/{u[side]}", tok=ROOT)
        time.sleep(4)
        for side in SIDES:
            cleanup_vm(u[side])


def run_clone(kind, u, table):
    destination = sql(f"SELECT sd.uuid FROM standalone_dockers sd JOIN servers s ON s.id=sd.server_id WHERE s.uuid='{SERVER}' ORDER BY sd.id LIMIT 1")
    clones = {}
    for side in SIDES:
        vm_out(f"docker exec {u[side]} sh -c {json.dumps(SEED[kind][0])}")
        s, r = call(side, "POST", f"/api/v1/databases/{u[side]}/clone", {"destination_uuid": destination, "clone_volumes": True, "name": "cl"}, ROOT)
        if s != 201:
            record(f"{kind}: clone on {side}", False, r)
            return
        clones[side] = json.loads(r)["uuid"]
    # Upstream's Bus chain fails on this reference after the stop (Horizon's job tags: "Nesting level
    # too deep"): its source stays stopped and the clone empty. The port does what the chain intends,
    # so this checks coolify-rs alone and brings upstream's source back for the next steps.
    time.sleep(15)
    s = wait_status(table, u["rs"])
    record(f"{kind}: source restarted after clone (coolify-rs)", s.startswith("running"), s)
    rid = sql(f"SELECT id FROM {table} WHERE uuid='{clones['rs']}'")
    vols = sql(f"SELECT name FROM local_persistent_volumes WHERE resource_type='App\\Models\\{MODELS[kind]}' AND resource_id={rid} ORDER BY id").splitlines()
    counts = [int(vm_out(f"docker run --rm -v {v}:/v alpine sh -c 'ls /v | wc -l'").strip() or 0) for v in vols]
    record(f"{kind}: clone has the volume data (coolify-rs)", any(c > 0 for c in counts), dict(zip(vols, counts)))
    call("ref", "POST", f"/api/v1/databases/{u['ref']}/start", tok=ROOT)
    wait_status(table, u["ref"])
    for side in SIDES:
        call(side, "DELETE", f"/api/v1/databases/{clones[side]}", tok=ROOT)


def run_ssl(kind, u, table, morph):
    for side in SIDES:
        sql(f"UPDATE {table} SET enable_ssl=true, custom_docker_run_options=NULL WHERE uuid='{u[side]}'")
        s, r = call(side, "POST", f"/api/v1/databases/{u[side]}/restart", tok=ROOT)
        if s != 200:
            record(f"{kind}: ssl restart on {side}", False, r)
    time.sleep(8)
    st = {s: wait_status(table, u[s], tries=90) for s in SIDES}
    record(f"{kind}: ssl started", all(v.startswith("running") for v in st.values()), st)
    same_file(f"{kind}: ssl compose file", lambda x: f"/data/coolify/databases/{x}/docker-compose.yml", u)
    rows = {}
    for side in SIDES:
        rid = sql(f"SELECT id FROM {table} WHERE uuid='{u[side]}'")
        text = sql(f"SELECT row_to_json(c)::text FROM (SELECT common_name, subject_alternative_names, is_ca_certificate, mount_path, configuration_dir, valid_until::date - now()::date AS days FROM ssl_certificates WHERE resource_type='{morph}' AND resource_id={rid}) c")
        rows[side] = norm(text, u.values())
    record(f"{kind}: certificate row", rows["ref"] == rows["rs"] and rows["ref"] != "", rows)
    files = {}
    for side in SIDES:
        rid = sql(f"SELECT id FROM {table} WHERE uuid='{u[side]}'")
        text = sql(f"SELECT string_agg(fs_path||' '||mount_path||' '||COALESCE(chmod,''), ',' ORDER BY mount_path) FROM local_file_volumes WHERE resource_type='{morph}' AND resource_id={rid}")
        files[side] = norm(text, u.values())
    record(f"{kind}: certificate file rows", files["ref"] == files["rs"], files)
    # The certificates themselves, as OpenSSL reads them.
    text = {}
    for side in SIDES:
        name = "server.pem" if kind == "mongodb" else "server.crt"
        out = vm_out(f"openssl x509 -in /data/coolify/databases/{u[side]}/ssl/{name} -noout -text -nameopt compat 2>&1")
        out = re.sub(r"Serial Number:.*?\n.*?\n", "", out)
        out = re.sub(r"Not (Before|After) *:.*", "", out)
        out = re.sub(r"pub:\n( +[0-9a-f:]+\n)+", "pub:\n", out)
        out = re.sub(r"Signature Value:\n( +[0-9a-f:]+\n?)+", "Signature\n", out)
        out = re.sub(r"X509v3 Subject Key Identifier: *\n.*\n", "SKI\n", out)
        text[side] = norm(out, u.values())
    record(f"{kind}: certificate fields", text["ref"] == text["rs"] and "Issuer" in text["ref"],
           "\n".join(difflib.unified_diff(text["ref"].splitlines(), text["rs"].splitlines(), "ref", "rs", lineterm=""))[:2000])
    # Both verify against the same CA of the server, inside the container.
    verify = {}
    for side in SIDES:
        name = "server.pem" if kind == "mongodb" else "server.crt"
        verify[side] = vm_out(f"openssl verify -CAfile /data/coolify/ssl/coolify-ca.crt /data/coolify/databases/{u[side]}/ssl/{name} 2>&1 | tail -1").replace(u[side], "UUID").strip()
    record(f"{kind}: certificate verifies with the CA", verify["ref"] == verify["rs"] and verify["ref"].endswith("OK"), verify)
    port = {"postgresql": (5432, "-starttls postgres"), "mysql": (3306, "-starttls mysql"), "mariadb": (3306, "-starttls mysql"), "mongodb": (27017, ""),
            "redis": (6380, ""), "keydb": (6380, ""), "dragonfly": (6379, "")}[kind]
    tls = {}
    for side in SIDES:
        ip = vm_out(f"docker inspect --format '{{{{range .NetworkSettings.Networks}}}}{{{{.IPAddress}}}}{{{{end}}}}' {u[side]}").strip()
        out = vm_out(f"echo | timeout 8 openssl s_client -connect {ip}:{port[0]} {port[1]} -CAfile /data/coolify/ssl/coolify-ca.crt 2>&1 | grep -E 'Verify return code' | head -1")
        tls[side] = out.strip()
    record(f"{kind}: TLS handshake", tls["ref"] == tls["rs"] and "0 (ok)" in tls["ref"], tls)


def run_backup(kind, u, table, s3):
    seed, read, wipe = SEED[kind]
    def exec_in(side, cmd):
        return vm_out(f"docker exec {u[side]} sh -c {json.dumps(cmd)} 2>&1")
    s3_uuid = {}
    for side in SIDES:
        s, r = call(side, "POST", "/api/v1/s3-storages", {"name": f"minio-{side}", "endpoint": s3, "bucket": "coolify-test", "region": "us-east-1", "key": "minioadmin", "secret": "minioadmin"}, ROOT)
        s3_uuid[side] = json.loads(r)["uuid"] if s == 201 else None
        if s != 201:
            record(f"{kind}: s3 storage on {side}", False, r)
            return
        exec_in(side, seed)
    v = {}
    for side in SIDES:
        s, r = call(side, "POST", f"/api/v1/s3-storages/{s3_uuid[side]}/validate", {}, ROOT)
        v[side] = (s, r)
    record(f"{kind}: s3 validate", v["ref"] == v["rs"] and b"true" in v["ref"][1], v)
    bid = {}
    try:
        for side in SIDES:
            s, r = call(side, "POST", f"/api/v1/databases/{u[side]}/backups",
                        {"frequency": "0 3 * * *", "save_s3": True, "s3_storage_uuid": s3_uuid[side], "backup_now": True, "database_backup_retention_amount_locally": 2}, ROOT)
            if s != 201:
                record(f"{kind}: backup on {side}", False, r)
                return
            bid[side] = json.loads(r)["uuid"]
        ex = {}
        for side in SIDES:
            for _ in range(90):
                rows = sql(f"SELECT row_to_json(e)::text FROM scheduled_database_backup_executions e JOIN scheduled_database_backups b ON b.id=e.scheduled_database_backup_id WHERE b.uuid='{bid[side]}' ORDER BY e.id DESC LIMIT 1")
                if rows and json.loads(rows)["status"] != "running":
                    ex[side] = json.loads(rows)
                    break
                time.sleep(2)
        if len(ex) < 2:
            record(f"{kind}: backup finished", False, ex)
            return
        keys = ["status", "database_name", "s3_uploaded", "local_storage_deleted", "s3_storage_deleted", "message"]
        guid = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
        def clean(e):
            return {k: guid.sub("<guid>", e[k]) if isinstance(e.get(k), str) else e.get(k) for k in keys}
        ca, cb = clean(ex["ref"]), clean(ex["rs"])
        record(f"{kind}: execution row", ca == cb and ca["status"] == "success", f"ref {ca}\n  rs  {cb}\n  ref file {ex['ref'].get('filename')}")
        def shape(side):
            return re.sub(r"\d{9,}-[a-z0-9]+", "T-U", ex[side]["filename"]).replace(u[side], "UUID")
        record(f"{kind}: backup file name", shape("ref") == shape("rs"), f"{shape('ref')} | {shape('rs')}")
        sizes = {s: int(ex[s]["size"] or 0) for s in SIDES}
        record(f"{kind}: backup has data", all(x > 0 for x in sizes.values()), sizes)
        in_s3 = {}
        for side in SIDES:
            key = ex[side]["filename"].lstrip("/")
            in_s3[side] = vm_out(f"docker exec coolify-rs-minio-test sh -c 'ls /data/coolify-test/{key} >/dev/null 2>&1 && echo yes || echo no'").strip()
        record(f"{kind}: file in S3", in_s3 == {"ref": "yes", "rs": "yes"}, in_s3)
        replicas = {s: sql(f"SELECT count(*), bool_and(s3_uploaded), bool_or(s3_storage_deleted) FROM database_backup_s3_replicas WHERE execution_id={ex[s]['id']}") for s in SIDES}
        record(f"{kind}: replica row", replicas["ref"] == replicas["rs"] and replicas["ref"].startswith("1|t"), replicas)
        lists = {}
        for side in SIDES:
            s, r = call(side, "GET", f"/api/v1/databases/{u[side]}/backups/{bid[side]}/executions", tok=ROOT)
            e = json.loads(r)["executions"][0]
            lists[side] = (s, list(e.keys()), e["status"], guid.sub("<guid>", e["message"] or ""))
        record(f"{kind}: executions list", lists["ref"] == lists["rs"], lists)
        # --- restore
        if kind in IMPORTS:
            for mode, payload in (("server", lambda side: {"source": "server", "path": ex[side]["filename"], "replace_existing": kind != "postgresql" or True}),
                                  ("s3", lambda side: {"source": "s3", "s3_storage_uuid": s3_uuid[side], "path": ex[side]["filename"], "replace_existing": True})):
                results = {}
                for side in SIDES:
                    if wipe:
                        exec_in(side, wipe)
                    s, r = call(side, "POST", f"/api/v1/databases/{u[side]}/imports", payload(side), ROOT)
                    if s != 202:
                        results[side] = (s, r)
                        continue
                    body = json.loads(r)
                    keys_ = list(body.keys())
                    st = None
                    for _ in range(90):
                        s2, r2 = call(side, "GET", body["status_url"], tok=ROOT)
                        st = json.loads(r2)
                        if st["status"] in ("finished", "error"):
                            break
                        time.sleep(2)
                    data = exec_in(side, read).strip().splitlines()
                    results[side] = (s, keys_, st["status"], st["exit_code"], list(st.keys()), data[-1] if data else "", st.get("output") if st["status"] == "error" else "")
                record(f"{kind}: restore from {mode}", results["ref"] == results["rs"] and results["ref"][2] == "finished" and "seed-1" in results["ref"][5], results)
            # upload
            results = {}
            for side in SIDES:
                if wipe:
                    exec_in(side, wipe)
                local = f"/tmp/dbs-upload-{side}"
                raw = subprocess.run(["orb", "-m", VM, "-u", "root", "cat", ex[side]["filename"]], capture_output=True).stdout
                open(local, "wb").write(raw)
                ext = {"postgresql": "dmp", "mysql": "sql", "mariadb": "sql", "mongodb": "archive.gz", "sqlite": "gz"}[kind]
                upload_id = sql("SELECT gen_random_uuid()")
                base = dd.BASES[side]
                out = subprocess.run(["curl", "-s", "-w", "\n%{http_code}", "-H", "Authorization: Bearer " + dd.TOKENS[ROOT], "-H", "Accept: application/json",
                                      "-F", f"upload_id={upload_id}", "-F", f"file=@{local};filename=b.{ext}", f"{base}/api/v1/databases/{u[side]}/imports/uploads"],
                                     capture_output=True, text=True).stdout
                os.unlink(local)
                body, code = out.rsplit("\n", 1)
                if code != "201":
                    results[side] = (code, body)
                    continue
                j = json.loads(body)
                s, r = call(side, "POST", f"/api/v1/databases/{u[side]}/imports", {"source": "upload", "upload_id": upload_id, "replace_existing": True}, ROOT)
                st = {}
                if s == 202:
                    url = json.loads(r)["status_url"]
                    for _ in range(90):
                        st = json.loads(call(side, "GET", url, tok=ROOT)[1])
                        if st["status"] in ("finished", "error"):
                            break
                        time.sleep(2)
                data = exec_in(side, read).strip().splitlines()
                results[side] = (code, list(j.keys()), j.get("filename"), j.get("mime_type"), s, st.get("status"), data[-1] if data else "")
            record(f"{kind}: restore from upload", results["ref"] == results["rs"] and "seed-1" in str(results["ref"]), results)
        # --- delete execution with S3, then the schedule
        d = {}
        for side in SIDES:
            s, r = call(side, "DELETE", f"/api/v1/databases/{u[side]}/backups/{bid[side]}/executions/{ex[side]['uuid']}?delete_s3=true", tok=ROOT)
            key = ex[side]["filename"].lstrip("/")
            d[side] = (s, r, vm_out(f"docker exec coolify-rs-minio-test sh -c 'ls /data/coolify-test/{key} >/dev/null 2>&1 && echo yes || echo no'").strip(),
                       vm_out(f"ls {ex[side]['filename']} 2>/dev/null").strip() == "")
        record(f"{kind}: execution deleted everywhere", d["ref"] == d["rs"] and d["ref"][2] == "no" and d["ref"][3], d)
        for side in SIDES:
            s, r = call(side, "DELETE", f"/api/v1/databases/{u[side]}/backups/{bid[side]}?delete_s3=true", tok=ROOT)
            bid.pop(side, None) if s == 200 else None
    finally:
        for side in SIDES:
            if side in bid:
                call(side, "DELETE", f"/api/v1/databases/{u[side]}/backups/{bid[side]}", tok=ROOT)
            if s3_uuid.get(side):
                call(side, "DELETE", f"/api/v1/s3-storages/{s3_uuid[side]}", tok=ROOT)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--s3", required=True)
    ap.add_argument("--kinds", default="postgresql,mysql,mariadb,mongodb,redis,keydb,dragonfly,clickhouse,sqlite")
    ap.add_argument("--parts", default="proxy,backup,clone,ssl")
    ap.add_argument("--port-base", dest="port_base", type=int, default=45100)
    a = ap.parse_args()
    a.parts = a.parts.split(",")
    project = json.loads(call("ref", "POST", "/api/v1/projects", {"name": "server-diff"}, ROOT)[1])["uuid"]
    try:
        for i, kind in enumerate(a.kinds.split(",")):
            a.port_base += 2
            print(f"--- {kind}", flush=True)
            run_kind(kind, a, project, a.s3)
    finally:
        time.sleep(8)
        call("ref", "DELETE", f"/api/v1/projects/{project}", tok=ROOT)
    passed = sum(dd.results)
    print(f"\n{passed}/{len(dd.results)} checks identical")
    sys.exit(0 if passed == len(dd.results) else 1)


if __name__ == "__main__":
    main()
