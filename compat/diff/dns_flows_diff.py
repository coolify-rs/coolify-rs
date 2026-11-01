#!/usr/bin/env python3
"""Managed DNS flows on both sides, against compat/diff/fake_providers.py only.

The reference runs its Livewire components and jobs under tinker with `Bus::fake` and `Http::fake`, so no
Horizon worker sees a job and no request leaves this machine. Its real provider URL is never contacted.
The resources live on a server row with the placeholder address 1.2.3.4 (public, skipped by the server
jobs, is_usable = false), so nothing connects to that address. A check of DNS only looks up .test names.

    python3 compat/diff/fake_providers.py 18191 &
    COOLIFY_RS_TEST_ONLY_FAKE_PROVIDERS=http://127.0.0.1:18191 scripts/dev-server.sh 18087
    COOLIFY_RS_BASE=http://127.0.0.1:18087 python3 compat/diff/dns_flows_diff.py

Every row is tracked by id and removed at the end. The hour change is avoided, because the reference's
scheduler runs dns:release-orphaned-records on the hour and must never see rows with the fake token.
"""
import base64
import datetime
import json
import os
import re
import subprocess
import sys
import time

from integrations_diff import ENVIRONMENT, FAKE, FAKE_PHP, PROJECT, RS, ROOT, TOKENS, compare, fake, fake_log, login, norm_requests, record, results, sql, team_id, tinker

created = {"apps": [], "services": [], "server": None, "dest": None, "tokens": []}
BIN = os.path.expanduser("~/.cache/coolify-rs-dev/18087/coolify-rs")


def guard_hour():
    while datetime.datetime.now().minute >= 50:
        print("waiting: too close to the hour for the reference's orphan schedule")
        time.sleep(60)


def create_server():
    php = f"""
$o = \\App\\Models\\Server::where('uuid', 'hml20fsj5eles0xfiqidq8j5')->first();
$s = \\App\\Models\\Server::create(['name' => 'diff-dns', 'ip' => '1.2.3.4', 'user' => 'root', 'port' => 22, 'team_id' => $o->team_id, 'private_key_id' => $o->private_key_id, 'proxy' => $o->proxy]);
$s->settings()->update(['is_reachable' => false, 'is_usable' => false]);
$d = $s->standaloneDockers()->first();
echo '<<<'.json_encode(['id' => $s->id, 'uuid' => $s->uuid, 'dest' => $d?->id]).'>>>';"""
    r = tinker(php, fake=False)
    created["server"] = r["id"]
    sql(f"""UPDATE servers SET proxy = '{{"type": "TRAEFIK", "status": "exited"}}' WHERE id = {r['id']}""")
    created["dest"] = r["dest"]
    return r


def create_token(name, token="cf-good", metadata=None):
    meta = json.dumps(metadata or {})
    php = f"""
$t = \\App\\Models\\IntegrationToken::query()->create(['team_id' => {team_id()}, 'provider' => 'cloudflare', 'name' => {json.dumps(name)},
  'token' => {json.dumps(token)}, 'capabilities' => ['dns'], 'metadata' => json_decode({json.dumps(meta)}, true)]);
echo '<<<'.json_encode(['id' => $t->id]).'>>>';"""
    tid = tinker(php, fake=False)["id"]
    created["tokens"].append(tid)
    return tid


def add_zone(tid, name="example.com", zone_id="zone-example"):
    sql(f"INSERT INTO dns_provider_zones (uuid, integration_token_id, provider_zone_id, name, account_id, created_at, updated_at) "
        f"VALUES (md5(random()::text), {tid}, '{zone_id}', '{name}', 'acct-1', now(), now())")


def create_app(sid, dest, name):
    php = f"""
$e = \\App\\Models\\Environment::where('uuid', '{ENVIRONMENT}')->first();
$a = new \\App\\Models\\Application(['name' => {json.dumps(name)}, 'repository_project_id' => 0, 'git_repository' => 'x/y', 'git_branch' => 'main',
  'build_pack' => 'dockerimage', 'ports_exposes' => '80', 'environment_id' => $e->id, 'destination_id' => {dest}, 'destination_type' => \\App\\Models\\StandaloneDocker::class,
  'docker_registry_image_name' => 'nginx', 'docker_registry_image_tag' => 'alpine']);
$a->save();
echo '<<<'.json_encode(['id' => $a->id, 'uuid' => $a->uuid]).'>>>';"""
    r = tinker(php, fake=False)
    created["apps"].append(r["id"])
    return r


def create_service(sid, dest, name):
    compose = "services:\n  web:\n    image: nginx:alpine\n"
    php = f"""
$e = \\App\\Models\\Environment::where('uuid', '{ENVIRONMENT}')->first();
$s = new \\App\\Models\\Service(['name' => {json.dumps(name)}, 'environment_id' => $e->id, 'server_id' => {sid}, 'destination_id' => {dest}, 'destination_type' => \\App\\Models\\StandaloneDocker::class,
  'docker_compose_raw' => {json.dumps(compose)}]);
$s->save();
$sa = \\App\\Models\\ServiceApplication::query()->create(['name' => 'web', 'service_id' => $s->id, 'image' => 'nginx:alpine']);
echo '<<<'.json_encode(['id' => $s->id, 'uuid' => $s->uuid, 'sub' => $sa->id]).'>>>';"""
    r = tinker(php, fake=False)
    created["services"].append(r["id"])
    return r


# What the components dispatch, without Livewire's own events.
LW = r"""
function lw_events($t) { $out = []; foreach (($t->effects['dispatches'] ?? []) as $d) { if (in_array($d['name'], ['success','error','warning','info'])) { $out[] = [$d['name'], $d['params'][0] ?? null]; } } return $out; }
\Illuminate\Support\Facades\Auth::login(\App\Models\User::where('email', 'alice@example.test')->first());
session(['currentTeam' => \App\Models\Team::find(TEAM)]);
\Illuminate\Support\Facades\Bus::fake([\App\Jobs\ConfigureDnsRecordJob::class, \App\Jobs\CheckDomainDnsJob::class, \App\Jobs\ReleaseRemovedDnsHostnamesJob::class, \App\Jobs\ReleaseManagedDnsRecordsJob::class]);
\Illuminate\Support\Facades\Event::fake([\App\Events\DnsRecordConfigurationFinished::class]);
"""


def lw_prelude():
    return LW.replace("TEAM", str(team_id()))


def run_jobs_php():
    """Runs the job the component queued, in this process, against the fake."""
    return r"""
$jobs = \Illuminate\Support\Facades\Bus::dispatched(\App\Jobs\ConfigureDnsRecordJob::class);
$finished = [];
foreach ($jobs as $j) { $j = is_array($j) ? $j[0] : $j; app()->call([$j, 'handle']); }
foreach (\Illuminate\Support\Facades\Event::dispatched(\App\Events\DnsRecordConfigurationFinished::class) as $e) { $e = $e[0]; $finished[] = ['successful' => $e->successful, 'message' => $e->message, 'credential' => $e->credential, 'hostname' => $e->hostname]; }
$checks = \Illuminate\Support\Facades\Bus::dispatched(\App\Jobs\CheckDomainDnsJob::class)->count();
"""


def ref_add_app(uuid, host):
    php = lw_prelude() + f"""
$a = \\App\\Models\\Application::where('uuid', '{uuid}')->first();
$t = \\Livewire\\Livewire::test(\\App\\Livewire\\Project\\Application\\Domains::class, ['application' => $a])
  ->set('newDomainParts', ['scheme' => 'https', 'host' => '{host}', 'port' => '', 'path' => ''])->set('newDomainPartsChanged', true)->call('addDomain');
$events = lw_events($t); $modal = $t->get('showDnsProviderModal');
""" + run_jobs_php() + "echo '<<<'.json_encode(['events' => $events, 'modal' => $modal, 'finished' => $finished, 'checks' => $checks]).'>>>';"
    return tinker(php)


def ref_add_service(sid, sub, host):
    php = lw_prelude() + f"""
$s = \\App\\Models\\Service::find({sid});
$t = \\Livewire\\Livewire::test(\\App\\Livewire\\Project\\Service\\Domains::class, ['service' => $s])
  ->set('newServiceApplicationId', {sub})->set('newDomainParts', ['scheme' => 'https', 'host' => '{host}', 'port' => '', 'path' => ''])->set('newDomainPartsChanged', true)->call('addDomain');
$events = lw_events($t); $modal = $t->get('showDnsProviderModal');
""" + run_jobs_php() + "echo '<<<'.json_encode(['events' => $events, 'modal' => $modal, 'finished' => $finished, 'checks' => $checks]).'>>>';"
    return tinker(php)


def rs_after(answer, kind, uuid, ui):
    """The same shape from the answer of coolify-rs."""
    dns = answer.get("dns") or {}
    events = [["warning", w] for w in answer.get("warnings") or []] + [["info", i] for i in dns.get("info") or []]
    if answer.get("message"):
        events.append(["success", answer["message"]])
    finished = []
    jobs = dns.get("jobs") or []
    for _ in range(30):
        if not jobs:
            break
        code, body = ui("GET", f"/ui/resources/{kind}/{uuid}/dns/finished?jobs={','.join(jobs)}")
        for f in body.get("finished", []):
            jobs = [j for j in jobs if j != f["job"]]
            finished.append({"successful": f["successful"], "message": f["message"]})
        finished.sort(key=lambda f: f["message"])
        time.sleep(0.5)
    return {"events": events, "modal": bool(dns.get("open_modal")), "finished": finished, "checks": None if dns else "started"}


def records(tid):
    return json.loads(sql(f"SELECT coalesce(json_agg(json_build_object('type', type, 'name', name, 'content', content, 'owned', owned, 'zone', (SELECT name FROM dns_provider_zones z WHERE z.id = r.dns_provider_zone_id), "
                          f"'refs', (SELECT coalesce(json_agg(x.resource_type || ':' || x.resource_id ORDER BY x.id), '[]') FROM managed_dns_record_references x WHERE x.managed_dns_record_id = r.id)) ORDER BY name), '[]') FROM managed_dns_records r WHERE integration_token_id = {tid}") or "[]")


def statuses(table, rid):
    raw = sql(f"SELECT domain_dns_statuses FROM {table} WHERE id = {rid}")
    if not raw:
        return None
    out = json.loads(raw)
    for v in out.values():
        for k in ("checked_at", "check_id"):
            v.pop(k, None)
    return out


def reset(table, rid, tid):
    sql(f"DELETE FROM managed_dns_record_references WHERE managed_dns_record_id IN (SELECT id FROM managed_dns_records WHERE integration_token_id = {tid})")
    sql(f"DELETE FROM managed_dns_records WHERE integration_token_id = {tid}")
    sql(f"UPDATE {table} SET fqdn = NULL, domain_dns_statuses = NULL, domain_port_overrides = NULL WHERE id = {rid}")
    fake("/__reset", "POST")


def side_flows(name, ui, table, rid, tid, ref_call, rs_call, kind, uuid, host="app.example.com"):
    reset(table, rid, tid)
    ref = ref_call(host)
    ref_req = norm_requests(fake_log())
    ref_rec, ref_st = records(tid), statuses(table, rid)
    ref_fqdn = sql(f"SELECT fqdn FROM {table} WHERE id = {rid}")
    reset(table, rid, tid)
    code, answer = rs_call(host)
    rs = rs_after(answer, kind, uuid, ui)
    time.sleep(1)
    rs_req = norm_requests(fake_log())
    rs_rec, rs_st = records(tid), statuses(table, rid)
    rs_fqdn = sql(f"SELECT fqdn FROM {table} WHERE id = {rid}")
    compare(f"{name}: toasts", ref["events"], rs["events"])
    compare(f"{name}: provider modal", ref["modal"], rs["modal"])
    compare(f"{name}: job outcome", sorted(({k: f[k] for k in ("successful", "message")} for f in ref["finished"]), key=lambda f: f["message"]), rs["finished"])
    # Queue workers run these jobs in any order, on both sides.
    key = lambda e: json.dumps(e, sort_keys=True)
    compare(f"{name}: requests", sorted(ref_req, key=key), sorted(rs_req, key=key))
    compare(f"{name}: records and references", ref_rec, rs_rec)
    compare(f"{name}: fqdn", ref_fqdn, rs_fqdn)
    return ref_st, rs_st, ref


def cleanup():
    for tid in created["tokens"]:
        sql(f"DELETE FROM managed_dns_record_references WHERE managed_dns_record_id IN (SELECT id FROM managed_dns_records WHERE integration_token_id = {tid})")
        sql(f"DELETE FROM managed_dns_records WHERE integration_token_id = {tid}")
        sql(f"DELETE FROM dns_provider_zones WHERE integration_token_id = {tid}")
        sql(f"DELETE FROM integration_tokens WHERE id = {tid}")
    for a in created["apps"]:
        sql(f"DELETE FROM application_settings WHERE application_id = {a}")
        sql(f"DELETE FROM applications WHERE id = {a}")
    for s in created["services"]:
        sql(f"DELETE FROM service_applications WHERE service_id = {s}")
        sql(f"DELETE FROM services WHERE id = {s}")
    if created["server"]:
        sql(f"DELETE FROM standalone_dockers WHERE server_id = {created['server']}")
        sql(f"DELETE FROM server_settings WHERE server_id = {created['server']}")
        sql(f"DELETE FROM servers WHERE id = {created['server']}")


def main():
    guard_hour()
    ui = login()
    try:
        srv = create_server()
        tid = create_token("dns2-auto")
        add_zone(tid)
        app = create_app(srv["id"], srv["dest"], "dns2-app")
        svc = create_service(srv["id"], srv["dest"], "dns2-svc")
        base = f"/ui/resources/application/{app['uuid']}/domains/add"

        def rs_app(host):
            return ui("POST", base, {"scheme": "https", "host": host, "port": "", "path": ""})

        def rs_svc(host):
            return ui("POST", f"/ui/services/{svc['uuid']}/domains", {"app_id": svc["sub"], "scheme": "https", "host": host, "port": "", "path": ""})

        # 1. One zone, automatic DNS: ConfigureDnsRecordJob.
        ref_st, rs_st, _ = side_flows("application, automatic", ui, "applications", app["id"], tid,
                                      lambda h: ref_add_app(app["uuid"], h), rs_app, "application", app["uuid"])
        # Upstream's status comes from the page's listener, which this run has no page for.
        record("application, automatic: no status stored upstream without an open page", ref_st is None, ref_st)
        record("application, automatic: the row is marked managed here", bool(rs_st) and all(v["status"] == "ok" for v in rs_st.values()), rs_st)
        side_flows("service, automatic", ui, "service_applications", svc["sub"], tid,
                   lambda h: ref_add_service(svc["id"], svc["sub"], h), rs_svc, "service", svc["uuid"])

        # 2. A conflict at the provider: the job reports it.
        def add_conflict():
            req = __import__("urllib.request").request.Request(FAKE + "/cloudflare/client/v4/zones/zone-example/dns_records", method="POST",
                                                               data=json.dumps({"type": "A", "name": "busy.example.com", "content": "9.9.9.9"}).encode())
            req.add_header("Authorization", "Bearer cf-good")
            __import__("urllib.request").request.urlopen(req).read()

        def ref_conflict(h):
            reset("applications", app["id"], tid)
            add_conflict()
            return ref_add_app(app["uuid"], h)

        def rs_conflict(h):
            reset("applications", app["id"], tid)
            add_conflict()
            return rs_app(h)
        side_flows("application, conflict", ui, "applications", app["id"], tid, ref_conflict, rs_conflict, "application", app["uuid"], "busy.example.com")

        # 3. Automatic DNS off: the provider modal.
        sql(f"UPDATE integration_tokens SET metadata = '{{\"automatic_dns\": false}}' WHERE id = {tid}")
        side_flows("application, automatic off", ui, "applications", app["id"], tid,
                   lambda h: ref_add_app(app["uuid"], h), rs_app, "application", app["uuid"])
        sql(f"UPDATE integration_tokens SET metadata = '{{}}' WHERE id = {tid}")

        # 4. Two tokens manage the zone: the provider modal.
        tid2 = create_token("dns2-second")
        add_zone(tid2)
        side_flows("application, two zones", ui, "applications", app["id"], tid,
                   lambda h: ref_add_app(app["uuid"], h), rs_app, "application", app["uuid"])
        sql(f"DELETE FROM dns_provider_zones WHERE integration_token_id = {tid2}")

        # 5. No zone: the DNS check starts. Checked here without running it on the reference.
        for label, ref_call, rs_call, kind, uuid in [
            ("application", lambda h: ref_add_app(app["uuid"], h), rs_app, "application", app["uuid"]),
            ("service", lambda h: ref_add_service(svc["id"], svc["sub"], h), rs_svc, "service", svc["uuid"]),
        ]:
            table, rid = ("applications", app["id"]) if kind == "application" else ("service_applications", svc["sub"])
            reset("applications", app["id"], tid)
            ref_st, rs_st, ref = side_flows(f"{label}, no zone", ui, table, rid, tid, ref_call, rs_call, kind, uuid, "dns2-none.example.test")
            record(f"{label}, no zone: CheckDomainDnsJob queued upstream for each status", ref["checks"] == len(ref_st or {}) and ref["checks"] > 0, (ref["checks"], ref["events"], ref_st))
            compare(f"{label}, no zone: status while checking", {k: {"status": v["status"], "expected_ip": v["expected_ip"]} for k, v in (ref_st or {}).items()},
                    {k: {"status": "checking", "expected_ip": v["expected_ip"]} for k, v in (rs_st or {}).items()})

        # 6. No public address: the warning, then the check.
        sql(f"UPDATE servers SET ip = '10.9.8.7' WHERE id = {srv['id']}")
        side_flows("application, private address", ui, "applications", app["id"], tid,
                   lambda h: ref_add_app(app["uuid"], h), rs_app, "application", app["uuid"])
        sql(f"UPDATE servers SET ip = '1.2.3.4' WHERE id = {srv['id']}")

        # 6b. CheckDomainDnsJob on both sides, on names that leak nothing (the settings' resolver is asked).
        urls = ["https://dns2-nx.example.test", "https://dns2-sub.1.2.3.4.sslip.io", "https://1.2.3.4", "https://dns2-b.example.test:8443/path"]
        for table, rid, kind, uuid, cls in [("applications", app["id"], "application", app["uuid"], "Application"),
                                            ("service_applications", svc["sub"], "service", svc["uuid"], "ServiceApplication")]:
            reset(table, rid, tid)
            sql(f"UPDATE {table} SET fqdn = '{','.join(urls)}' WHERE id = {rid}")
            php = f"""$m = \\App\\Models\\{cls}::find({rid}); $srv = \\App\\Models\\Server::find({srv['id']});
$urls = json_decode('{json.dumps(urls)}', true); $st = []; foreach ($urls as $u) {{ $st[$u] = ['status' => 'checking', 'message' => '', 'expected_ip' => '1.2.3.4', 'checked_at' => now()->toIso8601String(), 'check_id' => 'chk']; }}
$m->domain_dns_statuses = $st; $m->save();
foreach ($urls as $u) {{ (new \\App\\Jobs\\CheckDomainDnsJob($m->fresh(), $u, $u, $srv, '1.2.3.4', 'chk'))->handle(); }}
echo '<<<null>>>';"""
            tinker(php, fake=False)
            ref_st = statuses(table, rid)
            reset(table, rid, tid)
            sql(f"UPDATE {table} SET fqdn = '{','.join(urls)}' WHERE id = {rid}")
            code, _ = ui("POST", f"/ui/resources/{kind}/{uuid}/dns/check", {})
            for _i in range(40):
                rs_st = statuses(table, rid)
                if rs_st and all(v["status"] != "checking" for v in rs_st.values()):
                    break
                time.sleep(0.5)
            compare(f"{kind}: CheckDomainDns statuses", ref_st, rs_st)
            sql(f"UPDATE {table} SET fqdn = NULL, domain_dns_statuses = NULL WHERE id = {rid}")

        # 7. API PATCH: queueReleaseOfRemovedHostnames and ReleaseRemovedDnsHostnamesJob.
        def seed_record(side):
            reset("applications", app["id"], tid)
            if side == "ref":
                ref_add_app(app["uuid"], "app.example.com")
            else:
                rs_app("app.example.com")
                time.sleep(1.5)
            return len(fake_log())

        before = seed_record("rs")
        code, body = ui_api("PATCH", f"/applications/{app['uuid']}", {"domains": "https://other.example.test"})
        record("API edit: PATCH answers", code == 200, (code, body))
        for _ in range(20):
            if not records(tid):
                break
            time.sleep(0.5)
        rs_rec, rs_req = records(tid), norm_requests(fake_log()[before:])
        before = seed_record("ref")
        php = lw_prelude() + f"""
$a = \\App\\Models\\Application::find({app['id']});
$c = app(\\App\\Services\\Dns\\ManagedDnsRecordCleanup::class);
$p = $c->hostnamesOf($a); $a->fqdn = 'https://other.example.test'; $a->save(); $c->queueReleaseOfRemovedHostnames($a, $p, {team_id()});
$jobs = \\Illuminate\\Support\\Facades\\Bus::dispatched(\\App\\Jobs\\ReleaseRemovedDnsHostnamesJob::class);
foreach ($jobs as $j) {{ $j = is_array($j) ? $j[0] : $j; app()->call([$j, 'handle']); }}
echo '<<<'.json_encode(['jobs' => $jobs->count()]).'>>>';"""
        ref = tinker(php)
        ref_rec, ref_req = records(tid), norm_requests(fake_log()[before:])
        record("API edit: the job is queued upstream", ref["jobs"] == 1, ref)
        compare("API edit: records", ref_rec, rs_rec)
        key = lambda e: json.dumps(e, sort_keys=True)
        compare("API edit: provider requests", sorted(ref_req, key=key), sorted(rs_req, key=key))

        # 8. Nothing referenced: no job, no request.
        reset("applications", app["id"], tid)
        sql(f"UPDATE applications SET fqdn = 'https://none.example.test' WHERE id = {app['id']}")
        code, body = ui_api("PATCH", f"/applications/{app['uuid']}", {"domains": "https://none2.example.test"})
        time.sleep(1)
        compare("API edit without a record: no provider request", [], fake_log())

        # 9. dns:release-orphaned-records. Other rows in the table would make this unsafe to compare.
        foreign = sql(f"SELECT count(*) FROM managed_dns_records WHERE integration_token_id NOT IN ({','.join(str(t) for t in created['tokens'])})")
        if foreign != "0":
            print(f"SKIP  dns:release-orphaned-records: {foreign} rows of others in managed_dns_records")
        else:
            def seed_orphan(side):
                reset("applications", app["id"], tid)
                if side == "ref":
                    ref_add_app(app["uuid"], "orphan.example.com")
                else:
                    rs_app("orphan.example.com")
                    time.sleep(1.5)
                # The reference no longer exists: the resource was deleted without a release.
                sql(f"UPDATE managed_dns_record_references SET resource_id = -1 WHERE managed_dns_record_id IN (SELECT id FROM managed_dns_records WHERE integration_token_id = {tid})")
                sql(f"UPDATE applications SET fqdn = NULL WHERE id = {app['id']}")
                return len(fake_log())

            before = seed_orphan("ref")
            php = "$code = \\Illuminate\\Support\\Facades\\Artisan::call('dns:release-orphaned-records'); $out = trim(\\Illuminate\\Support\\Facades\\Artisan::output()); echo '<<<'.json_encode(['out' => $out]).'>>>';"
            ref = tinker(php)
            ref_req, ref_rec = norm_requests(fake_log()[before:]), records(tid)
            before = seed_orphan("rs")
            out = subprocess.run([BIN, "dns:release-orphaned-records"], capture_output=True, text=True, timeout=120, env=rs_env())
            rs_req, rs_rec = norm_requests(fake_log()[before:]), records(tid)
            compare("orphaned: output", ref["out"], out.stdout.strip().splitlines()[-1] if out.stdout.strip() else out.stderr[-300:])
            compare("orphaned: provider requests", sorted(ref_req, key=key), sorted(rs_req, key=key))
            compare("orphaned: records", ref_rec, rs_rec)
    finally:
        cleanup()
    bad = [n for n, ok in results if not ok]
    print(f"{len(results) - len(bad)}/{len(results)} same")
    sys.exit(1 if bad else 0)


def ui_api(method, path, body):
    import urllib.error
    import urllib.request
    req = urllib.request.Request(RS + "/api/v1" + path, data=json.dumps(body).encode(), method=method)
    req.add_header("Authorization", "Bearer " + TOKENS["acme_owner_root"])
    req.add_header("Content-Type", "application/json")
    req.add_header("Accept", "application/json")
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")


def rs_env():
    def value(container, name):
        return subprocess.run(["docker", "exec", container, "printenv", name], capture_output=True, text=True).stdout.strip()
    db = "coolify-rs-reference-postgres-1"
    env = dict(os.environ)
    env.update({
        "APP_KEY": value("coolify-rs-reference-coolify-1", "APP_KEY"),
        "DATABASE_URL": f"postgres://{value(db, 'POSTGRES_USER')}:{value(db, 'POSTGRES_PASSWORD')}@127.0.0.1:15432/{value(db, 'POSTGRES_DB')}",
        "COOLIFY_RS_TEST_ONLY_FAKE_PROVIDERS": FAKE,
        "COOLIFY_RS_SKIP_STARTUP_CLEANUP": "1",
        "COOLIFY_RS_SCHEDULER": "off",
        "COOLIFY_RS_DATA_DIR": os.path.expanduser("~/.cache/coolify-rs-dev/18087/data"),
    })
    return env


if __name__ == "__main__":
    main()
