#!/usr/bin/env python3
"""Differential check of integration tokens: secret managers and Cloudflare.

The reference cannot be pointed at a fake (its provider URLs are hard-coded and `SafeExternalUrl`
refuses local addresses), so its services run under tinker with `Http::fake` forwarding every request
to compat/diff/fake_providers.py: the real PHP runs, nothing leaves this machine. coolify-rs runs with
COOLIFY_RS_TEST_ONLY_FAKE_PROVIDERS pointing at the same fake. Both sides' requests, results and rows
are compared.

    python3 compat/diff/fake_providers.py 18191 &
    COOLIFY_RS_TEST_ONLY_FAKE_PROVIDERS=http://127.0.0.1:18191 scripts/dev-server.sh 18091
    COOLIFY_RS_BASE=http://127.0.0.1:18091 python3 compat/diff/integrations_diff.py

Every row it creates is tracked by id and deleted at the end, also after a crash.
"""
import base64
import http.cookiejar
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
TOKENS = json.load(open(os.path.join(ROOT, "compat/reference/secrets/tokens.json")))
RS = os.environ.get("COOLIFY_RS_BASE", "http://127.0.0.1:18091")
FAKE = "http://127.0.0.1:18191"
FAKE_FROM_REF = "http://host.docker.internal:18191"
PROJECT = "j8xm2emlllhfnyebjzgyzp7z"
ENVIRONMENT = "uj3st9yvtvzxh7rul3c3opcz"
SERVER = "hml20fsj5eles0xfiqidq8j5"
results = []
created = {"integration_tokens": [], "applications": [], "servers": []}


def record(name, same, detail=""):
    results.append((name, same))
    print(("same  " if same else "DIFF  ") + name + ("" if same else "\n      " + str(detail)[:1500]))


def compare(name, ref, rs):
    record(name, ref == rs, f"ref={json.dumps(ref, sort_keys=True)}\n      rs ={json.dumps(rs, sort_keys=True)}")


def sql(q):
    out = subprocess.run(["docker", "exec", "coolify-rs-reference-postgres-1", "psql", "-U", "coolify", "-d", "coolify", "-At", "-c", q],
                         capture_output=True, text=True, timeout=120)
    if out.returncode:
        raise SystemExit(out.stderr)
    return out.stdout.strip()


FAKE_PHP = r"""
\Illuminate\Support\Facades\Http::preventStrayRequests();
\Illuminate\Support\Facades\Http::fake(function ($req) {
    $url = $req->url();
    foreach (['https://api.cloudflare.com' => '/cloudflare', 'https://api.doppler.com' => '/doppler'] as $from => $to) {
        if (str_starts_with($url, $from)) { $url = 'FAKE'.$to.substr($url, strlen($from)); }
    }
    if (! str_starts_with($url, 'FAKE')) { throw new \RuntimeException('stray '.$url); }
    $headers = array_filter(['Authorization' => $req->header('Authorization')[0] ?? null, 'Accept' => 'application/json',
        'X-Vault-Token' => $req->header('X-Vault-Token')[0] ?? null, 'X-Vault-Namespace' => $req->header('X-Vault-Namespace')[0] ?? null,
        'Content-Type' => $req->header('Content-Type')[0] ?? null]);
    $r = (new \GuzzleHttp\Client(['http_errors' => false]))->request($req->method(), $url, ['headers' => $headers, 'body' => $req->body()]);
    return \Illuminate\Support\Facades\Http::response((string) $r->getBody(), $r->getStatusCode());
});
""".replace("FAKE", FAKE_FROM_REF)


def tinker(php, fake=True):
    code = (FAKE_PHP if fake else "") + php
    out = subprocess.run(["docker", "exec", "coolify-rs-reference-coolify-1", "php", "/var/www/html/artisan", "tinker", "--execute", code],
                         capture_output=True, text=True, timeout=120)
    marker = "<<<"
    if marker not in out.stdout:
        raise SystemExit(f"tinker failed: {out.stdout[-800:]} {out.stderr[-800:]}")
    return json.loads(out.stdout.split(marker, 1)[1].split(">>>", 1)[0])


def fake(path, method="GET"):
    req = urllib.request.Request(FAKE + path, method=method, data=b"" if method == "POST" else None)
    with urllib.request.urlopen(req) as r:
        return json.loads(r.read() or b"null")


def fake_log():
    """The requests, without what differs by client (the fake's own origin is already the same)."""
    return [{k: e[k] for k in ("method", "path", "query", "body", "auth", "vault_token", "vault_namespace")} for e in fake("/__log")]


def login():
    jar = http.cookiejar.CookieJar()
    opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
    text = open(os.path.join(ROOT, "compat/reference/secrets/ui-login.txt")).read()
    password = re.search(r"^password:\s*(.+)$", text, re.M)[1].strip()
    csrf = {}

    def call(method, path, body=None):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(RS + path, data=data, method=method)
        req.add_header("Content-Type", "application/json")
        req.add_header("Accept", "application/json")
        if csrf.get("t"):
            req.add_header("X-CSRF-Token", csrf["t"])
        try:
            with opener.open(req, timeout=60) as r:
                return r.status, json.loads(r.read() or b"null")
        except urllib.error.HTTPError as e:
            raw = e.read()
            try:
                return e.code, json.loads(raw)
            except ValueError:
                return e.code, raw.decode("utf-8", "replace")[:300]

    code, body = call("POST", "/ui/session", {"email": "alice@example.test", "password": password})
    if code != 200:
        raise SystemExit(f"login failed: {code}")
    csrf["t"] = body["csrf_token"]
    return call


def api(method, path, body=None, tok="acme_owner_root"):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(RS + "/api/v1" + path, data=data, method=method)
    req.add_header("Authorization", "Bearer " + TOKENS[tok])
    req.add_header("Content-Type", "application/json")
    req.add_header("Accept", "application/json")
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")


def token_id(uuid):
    return int(sql(f"SELECT id FROM integration_tokens WHERE uuid = '{uuid}'"))


def team_id():
    return int(sql(f"SELECT team_id FROM projects WHERE uuid = '{PROJECT}'"))


def ref_create_token(provider, name, token, caps, metadata):
    php = f"""
$t = \\App\\Models\\IntegrationToken::query()->create(['team_id' => {team_id()}, 'provider' => {json.dumps(provider)}, 'name' => {json.dumps(name)},
  'token' => {json.dumps(token)}, 'capabilities' => {json.dumps(caps)}, 'metadata' => json_decode({json.dumps(json.dumps(metadata))}, true)]);
echo '<<<'.json_encode(['id' => $t->id, 'uuid' => $t->uuid]).'>>>';"""
    r = tinker(php, fake=False)
    created["integration_tokens"].append(r["id"])
    return r


def cleanup():
    for app in created["applications"]:
        sql(f"DELETE FROM secret_manager_links WHERE resourceable_type = 'App\\Models\\Application' AND resourceable_id = {app}")
        sql(f"DELETE FROM environment_variables WHERE resourceable_type = 'App\\Models\\Application' AND resourceable_id = {app}")
        sql(f"DELETE FROM application_settings WHERE application_id = {app}")
        sql(f"DELETE FROM applications WHERE id = {app}")
    for sid in created["servers"]:
        sql(f"UPDATE server_settings SET cloudflare_tunnel_id = NULL, cloudflare_tunnel_integration_token_id = NULL WHERE server_id = {sid}")
    for tid in created["integration_tokens"]:
        sql(f"DELETE FROM managed_dns_record_references WHERE managed_dns_record_id IN (SELECT id FROM managed_dns_records WHERE integration_token_id = {tid})")
        sql(f"DELETE FROM managed_dns_records WHERE integration_token_id = {tid}")
        sql(f"DELETE FROM dns_provider_zones WHERE integration_token_id = {tid}")
        sql(f"DELETE FROM integration_tokens WHERE id = {tid}")
    for sid in created["servers"]:
        sql(f"DELETE FROM standalone_dockers WHERE server_id = {sid}")
        sql(f"DELETE FROM server_settings WHERE server_id = {sid}")
        sql(f"DELETE FROM servers WHERE id = {sid}")


def create_app(name):
    code, body = api("POST", "/applications/dockerimage", {
        "project_uuid": PROJECT, "environment_uuid": ENVIRONMENT, "server_uuid": SERVER,
        "docker_registry_image_name": "nginx", "docker_registry_image_tag": "alpine", "ports_exposes": "80", "name": name,
        "instant_deploy": False,
    })
    if code != 201:
        raise SystemExit(f"create application: {code} {body}")
    app_id = int(sql(f"SELECT id FROM applications WHERE uuid = '{body['uuid']}'"))
    created["applications"].append(app_id)
    return body["uuid"], app_id


SOURCES = {
    "doppler-service": ("doppler", "dp.st.good", {}, {}),
    "doppler-account": ("doppler", "dp.sa.good", {}, {"project": "proj", "config": "prd"}),
    "doppler-bad": ("doppler", "dp.st.bad", {}, {}),
    "infisical": ("infisical", "good-secret", {"client_id": "client-1", "base_url": "/infisical/"},
                  {"project_id": "p1", "environment": "prod", "secret_path": "/app"}),
    "infisical-old": ("infisical", "good-secret", {"client_id": "client-1", "base_url": "/infisical-old"},
                      {"project_id": "p1", "environment": "prod"}),
    "infisical-bad": ("infisical", "wrong", {"client_id": "client-1", "base_url": "/infisical"}, {"project_id": "p", "environment": "e"}),
    "vault": ("vault", "vault-good", {"base_url": "/vault", "namespace": "team-a", "timeout": 7}, {"mount": "secret", "path": "/app/x/"}),
    "vault-denied": ("vault", "vault-bad", {"base_url": "/vault"}, {"mount": "secret", "path": "app/x"}),
    "vault-missing": ("vault", "vault-good", {"base_url": "/vault"}, {"mount": "secret", "path": "none"}),
}


def with_base(meta, origin):
    out = dict(meta)
    if "base_url" in out:
        out["base_url"] = origin + out["base_url"]
    return out


def ref_fetch(provider, token, meta, settings):
    """`new XService(...)` directly: `SafeExternalUrl` refuses a local base URL, which is the one thing not compared here."""
    meta = with_base(meta, FAKE_FROM_REF)
    php = f"""
$meta = json_decode({json.dumps(json.dumps(meta))}, true); $s = json_decode({json.dumps(json.dumps(settings))}, true); $tok = {json.dumps(token)}; $p = {json.dumps(provider)};
$bypass = function (string $class, array $args) {{ $o = (new \\ReflectionClass($class))->newInstanceWithoutConstructor();
  $r = new \\ReflectionClass($class); foreach ($args as $k => $v) {{ $x = $r->getProperty($k); $x->setAccessible(true); $x->setValue($o, $v); }} return $o; }};
$svc = match ($p) {{
  'doppler' => new \\App\\Services\\DopplerService($tok),
  'infisical' => $bypass(\\App\\Services\\InfisicalService::class, ['baseUrl' => rtrim($meta['base_url'], '/'), 'clientId' => $meta['client_id'], 'clientSecret' => $tok, 'timeout' => $meta['timeout'] ?? null, 'httpClientOptions' => []]),
  'vault' => $bypass(\\App\\Services\\VaultService::class, ['baseUrl' => rtrim($meta['base_url'], '/'), 'token' => $tok, 'namespace' => $meta['namespace'] ?? null, 'timeout' => $meta['timeout'] ?? null, 'httpClientOptions' => []]),
}};
$valid = $svc->validate();
try {{
  $v = match ($p) {{
    'doppler' => $svc->fetchSecrets($s['project'] ?? null, $s['config'] ?? null),
    'infisical' => $svc->fetchSecrets((string) ($s['project_id'] ?? ''), (string) ($s['environment'] ?? ''), (string) ($s['secret_path'] ?? '/')),
    'vault' => $svc->fetchSecrets((string) ($s['mount'] ?? 'secret'), (string) ($s['path'] ?? '')),
  }};
  ksort($v);
  $res = ['ok' => $v];
}} catch (\\Throwable $e) {{ $res = ['error' => $e->getMessage()]; }}
echo '<<<'.json_encode(['valid' => $valid, 'result' => $res]).'>>>';"""
    return tinker(php)


def norm_comment(rows):
    out = []
    for r in rows:
        r = dict(r)
        if r.get("comment"):
            r["comment"] = re.sub(r"managed-by: coolify \S+/\S+", "managed-by: coolify ID/UUID", r["comment"])
        r.pop("id", None)
        out.append(r)
    return out


def norm_requests(items):
    out = []
    for e in items:
        e = json.loads(json.dumps(e))
        e["path"] = re.sub(r"(tunnel|rec)-\d+", r"\1-N", e["path"])
        if isinstance(e.get("body"), dict):
            for k in ("comment",):
                if e["body"].get(k):
                    e["body"][k] = re.sub(r"managed-by: coolify \S+/\S+", "managed-by: coolify ID/UUID", e["body"][k])
            if e["body"].get("content"):
                e["body"]["content"] = re.sub(r"tunnel-\d+", "tunnel-N", e["body"]["content"])
            if "name" in e["body"]:
                e["body"]["name"] = re.sub(r"coolify-[a-z0-9]{20,}", "coolify-SERVER", e["body"]["name"])
        if e["query"].get("name"):
            e["query"]["name"] = re.sub(r"coolify-[a-z0-9]{20,}", "coolify-SERVER", e["query"]["name"])
        out.append(e)
    return out


def tunnel_rows(sid, tid):
    row = sql(f"SELECT json_build_object('tunnel', cloudflare_tunnel_id IS NOT NULL, 'account', cloudflare_tunnel_account_id, 'token', cloudflare_tunnel_integration_token_id) FROM server_settings WHERE server_id = {sid}")
    recs = sql(f"SELECT coalesce(json_agg(json_build_object('type', type, 'name', name, 'content', regexp_replace(content, 'tunnel-[0-9]+', 'tunnel-N'), 'owned', owned, 'refs', (SELECT count(*) FROM managed_dns_record_references x WHERE x.managed_dns_record_id = r.id)) ORDER BY name), '[]') FROM managed_dns_records r WHERE integration_token_id = {tid}")
    return {"settings": json.loads(row), "records": json.loads(recs)}


def ref_tunnel(sid, tid, confirmed=None):
    conf = "null" if confirmed is None else json.dumps(json.dumps(confirmed))
    php = f"""
$s = \\App\\Models\\Server::find({sid}); $t = \\App\\Models\\IntegrationToken::find({tid}); $c = {conf} === null ? null : json_decode({conf}, true);
try {{ app(\\App\\Services\\CloudflareTunnelService::class)->setUp($s, $t, 'ssh.example.com', $c); $r = ['started' => true]; }}
catch (\\App\\Exceptions\\DnsRecordConflictException $e) {{ $r = ['record_id' => $e->providerRecordId, 'current' => $e->currentValue]; }}
catch (\\Throwable $e) {{ $r = ['error' => $e->getMessage()]; }}
echo '<<<'.json_encode($r).'>>>';"""
    return tinker(php)


def ref_teardown(sid):
    php = f"$s = \\App\\Models\\Server::find({sid}); echo '<<<'.json_encode(app(\\App\\Services\\CloudflareTunnelService::class)->tearDown($s)).'>>>';"
    return tinker(php)


TUNNEL_PREEXISTING = {"id": ""}


def vm_cloudflared_id():
    out = subprocess.run(["orb", "-m", "coolify-rs-e2e", "-u", "root", "bash", "-c", "docker ps -aq --no-trunc --filter name=^/coolify-cloudflared$"],
                         capture_output=True, text=True, timeout=120)
    return out.stdout.strip()


def reset_tunnel(sid, tid):
    # The VM is shared. tunnel_section() runs only without a container of this name, so one that
    # exists now came from this run; it is removed by id, and never one the run did not start with.
    container = vm_cloudflared_id()
    if container and container != TUNNEL_PREEXISTING["id"]:
        subprocess.run(["orb", "-m", "coolify-rs-e2e", "-u", "root", "docker", "rm", "-f", container], capture_output=True)
    sql(f"UPDATE server_settings SET cloudflare_tunnel_id = NULL, cloudflare_tunnel_account_id = NULL, cloudflare_tunnel_integration_token_id = NULL WHERE server_id = {sid}")
    sql(f"DELETE FROM managed_dns_record_references WHERE managed_dns_record_id IN (SELECT id FROM managed_dns_records WHERE integration_token_id = {tid})")
    sql(f"DELETE FROM managed_dns_records WHERE integration_token_id = {tid}")


def tunnel_section(ui, tid):
    """`CloudflareTunnelService` on a server row of its own: the shared test server is never edited."""
    TUNNEL_PREEXISTING["id"] = vm_cloudflared_id()
    if TUNNEL_PREEXISTING["id"]:
        print("SKIP  tunnel section: coolify-cloudflared already exists on the VM and was not created here")
        return
    php = f"""
$o = \\App\\Models\\Server::where('uuid', '{SERVER}')->first();
$s = \\App\\Models\\Server::create(['name' => 'diff-tunnel', 'ip' => $o->ip, 'user' => $o->user, 'port' => $o->port, 'team_id' => $o->team_id, 'private_key_id' => $o->private_key_id, 'proxy' => $o->proxy]);
$s->settings()->update(['is_reachable' => true, 'is_usable' => true]);
echo '<<<'.json_encode(['id' => $s->id, 'uuid' => $s->uuid]).'>>>';"""
    created_server = tinker(php, fake=False)
    created["servers"].append(created_server["id"])
    sid, suuid = created_server["id"], created_server["uuid"]
    url = f"/ui/server-page/{suuid}/cloudflare-tunnel"
    body = {"cloudflare_integration_token_id": tid, "api_ssh_domain": "ssh.example.com"}

    def side(name, ref_fn, rs_fn):
        reset_tunnel(sid, tid)
        fake("/__reset", "POST")
        pre = setup_conflict.get(name)
        if pre:
            pre()
        before = len(fake_log())
        ref = ref_fn()
        ref_req = fake_log()[before:]
        ref_rows = tunnel_rows(sid, tid)
        ref_records = fake("/__records")
        reset_tunnel(sid, tid)
        fake("/__reset", "POST")
        if pre:
            pre()
        before = len(fake_log())
        rs = rs_fn()
        rs_req = fake_log()[before:]
        rs_rows = tunnel_rows(sid, tid)
        rs_records = fake("/__records")
        return ref, ref_req, ref_rows, ref_records, rs, rs_req, rs_rows, rs_records

    def add_conflict():
        req = urllib.request.Request(FAKE + "/cloudflare/client/v4/zones/zone-example/dns_records", method="POST",
                                     data=json.dumps({"type": "A", "name": "ssh.example.com", "content": "9.9.9.9"}).encode())
        req.add_header("Authorization", "Bearer cf-good")
        req.add_header("Content-Type", "application/json")
        urllib.request.urlopen(req).read()

    setup_conflict = {"conflict": add_conflict, "replace": add_conflict}

    # A new tunnel.
    ref, rq, rr, rd, rs, sq, sr, sd = side("new", lambda: ref_tunnel(sid, tid), lambda: ui("POST", url + "/api", body))
    compare("setUp: started", ref.get("started"), rs[0] == 200 and "activity_id" in rs[1])
    compare("setUp: requests", norm_requests(rq), norm_requests(sq))
    compare("setUp: rows", rr, sr)
    compare("setUp: records at the provider", norm_comment(rd), norm_comment(sd))

    # A conflicting record.
    ref, rq, rr, rd, rs, sq, sr, sd = side("conflict", lambda: ref_tunnel(sid, tid), lambda: ui("POST", url + "/api", body))
    compare("setUp conflict: answer", ref.get("current"), rs[1].get("dns_conflict", {}).get("current") if rs[0] == 200 else rs)
    compare("setUp conflict: requests", norm_requests(rq), norm_requests(sq))
    compare("setUp conflict: rows", rr, sr)

    # Confirmed: replaces the record.
    rec = lambda: [r for r in fake("/__records") if r["name"] == "ssh.example.com"][0]["id"]
    ref, rq, rr, rd, rs, sq, sr, sd = side(
        "replace",
        lambda: ref_tunnel(sid, tid, {"record_id": rec(), "current": "9.9.9.9"}),
        lambda: ui("POST", url + "/api", {**body, "confirmed_conflict": {"record_id": rec(), "current": "9.9.9.9"}}),
    )
    compare("setUp replace: started", ref.get("started"), rs[0] == 200 and "activity_id" in rs[1])
    compare("setUp replace: requests", norm_requests(rq), norm_requests(sq))
    compare("setUp replace: rows", rr, sr)
    compare("setUp replace: records at the provider", norm_comment(rd), norm_comment(sd))

    # tearDown after a setUp.
    def teardown_of(setup, down):
        reset_tunnel(sid, tid)
        fake("/__reset", "POST")
        setup()
        time.sleep(1)
        before = len(fake_log())
        out = down()
        return out, fake_log()[before:], tunnel_rows(sid, tid), fake("/__records")

    ref, rq, rr, rd = teardown_of(lambda: ref_tunnel(sid, tid), lambda: ref_teardown(sid))
    rs, sq, sr, sd = teardown_of(lambda: ui("POST", url + "/api", body), lambda: ui("POST", url + "/disable"))
    compare("tearDown: warnings", ref, rs[1].get("warnings") if rs[0] == 200 else rs)
    compare("tearDown: requests", norm_requests(rq), norm_requests(sq))
    compare("tearDown: rows", rr, sr)
    compare("tearDown: records at the provider", rd, sd)
    reset_tunnel(sid, tid)

    # Refused by the token.
    r = ref_create_token("cloudflare", "diff-cf-tunnel-denied", "cf-dnsonly", ["dns", "tunnel"], {})
    sql(f"INSERT INTO dns_provider_zones (uuid, integration_token_id, provider_zone_id, name, account_id, account_name, created_at, updated_at) VALUES ('zdenied{r['id']}', {r['id']}, 'zone-example', 'example.com', 'acct-1', 'Acme', now(), now())")
    fake("/__reset", "POST")
    ref_answer = ref_tunnel(sid, r["id"])
    code, body2 = ui("POST", url + "/api", {"cloudflare_integration_token_id": r["id"], "api_ssh_domain": "ssh.example.com"})
    compare("setUp 403: message", ref_answer.get("error"), body2.get("message") if isinstance(body2, dict) else body2)
    reset_tunnel(sid, r["id"])


def uncompromised_section(ui):
    """`uncompromised()` through the fake: a leaked password (`password`'s hash suffix is in the fake's range)
    needs the rule's own text. The reference's validator runs under tinker with `Http::fake`."""
    php = r"""
\Illuminate\Support\Facades\Http::preventStrayRequests();
\Illuminate\Support\Facades\Http::fake(['api.pwnedpasswords.com/*' => function ($req) {
    $r = (new \GuzzleHttp\Client(['http_errors' => false]))->request('GET', 'http://host.docker.internal:18191/hibp/range/'.basename($req->url()));
    return \Illuminate\Support\Facades\Http::response((string) $r->getBody(), $r->getStatusCode());
}]);
$out = [];
foreach (['password', 'Abcdefg1!x', 'Pa55word!Leak'] as $p) {
  $rule = \Illuminate\Validation\Rules\Password::min(8)->mixedCase()->letters()->numbers()->symbols()->uncompromised();
  $v = \Illuminate\Support\Facades\Validator::make(['password' => $p], ['password' => [$rule]]);
  $out[$p] = $v->errors()->get('password');
}
echo '<<<'.json_encode($out).'>>>';"""
    ref = tinker(php, fake=False)
    # On coolify-rs through a password reset form validation is not reachable without a token, so the rule
    # runs through the profile page, which answers before it changes anything when the current password is wrong.
    rs = {}
    for p in ["password", "Abcdefg1!x", "Pa55word!Leak"]:
        code, body = ui("POST", "/ui/profile/password", {"current_password": "not-the-password", "new_password": p, "new_password_confirmation": p})
        errors = body.get("errors", {}) if isinstance(body, dict) else {}
        rs[p] = [e.replace("new password", "password") for e in errors.get("new_password", [])]
    compare("uncompromised: messages", ref, rs)
    fake("/__reset", "POST")
    ui("POST", "/ui/profile/password", {"current_password": "not-the-password", "new_password": "Abcdefg1!x", "new_password_confirmation": "Abcdefg1!x"})
    log = fake_log()
    compare("uncompromised: the request has only the hash prefix", [(e["method"], e["path"]) for e in log], [("GET", "/hibp/range/" + __import__("hashlib").sha1(b"Abcdefg1!x").hexdigest()[:5].upper())])


def main():
    ui = login()
    # The formatters: PHP against the Rust unit tests' table, through a deployment-free path.
    php = r"""
$vals = ["plain", 'a$b', "it's", "tail\\", 'q"\'z', "multi\nline", '#x', ''];
$out = [];
foreach ($vals as $v) { $out[] = [\App\Support\RemoteSecretValueFormatter::dotenv($v), \App\Support\RemoteSecretValueFormatter::composeFile($v)]; }
echo '<<<'.json_encode($out).'>>>';"""
    ref_formats = tinker(php, fake=False)
    expected = [["'plain'", "plain"], ["'a$b'", "a$$b"], ['"it\'s"', "it's"], ['"tail\\\\"', "tail\\"],
                ['"q\\"\'z"', 'q"\'z'], ["'multi\nline'", "multi\nline"], ["'#x'", "#x"], ["''", ""]]
    compare("RemoteSecretValueFormatter table (the same cases as the Rust tests)", ref_formats, expected)

    for name, (provider, token, meta, settings) in SOURCES.items():
        fake("/__reset", "POST")
        ref = ref_fetch(provider, token, meta, settings)
        ref_requests = fake_log()
        fake("/__reset", "POST")
        # coolify-rs: create the token through the page endpoint (runs the validator).
        rs_meta = with_base(meta, FAKE)
        body = {"provider": provider, "name": f"diff-{name}", "token": token, "capabilities": ["secrets"], "metadata": rs_meta}
        code, created_body = ui("POST", "/ui/security/integration-tokens", body)
        rs_valid = code == 200
        rs_requests_validate = fake_log()
        rs = {"valid": rs_valid}
        if rs_valid:
            uuid = sql(f"SELECT uuid FROM integration_tokens WHERE name = 'diff-{name}' ORDER BY id DESC LIMIT 1")
            tid = token_id(uuid)
            created["integration_tokens"].append(tid)
        else:
            # A refused token: store the row directly with the reference's encryption so the fetch can run.
            r = ref_create_token(provider, f"diff-{name}", token, ["secrets"], rs_meta)
            uuid, tid = r["uuid"], r["id"]
        app_uuid, app_id = create_app(f"diff-int-{name}")
        code, saved = ui("PUT", f"/ui/resources/application/{app_uuid}/secret-manager", {"integration_token_uuid": uuid})
        record(f"{name}: link saved", code == 200 and saved.get("saved") is True, saved)
        if settings:
            code, saved = ui("POST", f"/ui/resources/application/{app_uuid}/secret-manager/settings", {"settings": settings})
            record(f"{name}: settings saved", code == 200, saved)
        fake("/__reset", "POST")
        code, keys = ui("POST", f"/ui/resources/application/{app_uuid}/secret-manager/keys")
        rs_requests_fetch = fake_log()
        if code == 200:
            rs["result_keys"] = sorted(k["key"] for k in keys["keys"])
        else:
            rs["error"] = keys.get("message", "").removeprefix("Could not fetch keys: ") if isinstance(keys, dict) else keys
        ref_view = {"valid": ref["valid"]}
        if "ok" in ref["result"]:
            ref_view["result_keys"] = sorted(ref["result"]["ok"].keys())
        else:
            ref_view["error"] = ref["result"]["error"]
        compare(f"{name}: validate and fetch", ref_view, rs)
        compare(f"{name}: provider requests", ref_requests, rs_requests_validate + rs_requests_fetch)
        # The summary and the import as the model computes them, on the row coolify-rs wrote.
        php = f"""
$l = \\App\\Models\\SecretManagerLink::query()->where('resourceable_type', 'App\\\\Models\\\\Application')->where('resourceable_id', {app_id})->first();
echo '<<<'.json_encode(['summary' => $l->sourceSummary(), 'settings' => $l->settings]).'>>>';"""
        ref_row = tinker(php, fake=False)
        code, st = ui("GET", f"/ui/resources/application/{app_uuid}/secret-manager")
        compare(f"{name}: source summary and settings", {**ref_row, "settings": ref_row["settings"] or None}, {"summary": st["source_summary"], "settings": st["settings"] or None})
        if code == 200 and "result_keys" in rs:
            code, imp = ui("POST", f"/ui/resources/application/{app_uuid}/secret-manager/import")
            rows = sql(f"SELECT coalesce(json_agg(key ORDER BY key), '[]') FROM environment_variables WHERE resourceable_id = {app_id} AND resourceable_type = 'App\\Models\\Application' AND is_preview = false")
            php = f"""
$keys = json_decode({json.dumps(json.dumps(rs['result_keys']))}, true); $imported = []; $skipped = [];
foreach ($keys as $k) {{ if (! \\App\\Support\\ValidationPatterns::isValidEnvironmentVariableKey($k)) {{ $skipped[] = $k; }} else {{ $imported[] = $k; }} }}
sort($imported);
echo '<<<'.json_encode(['keys' => $imported]).'>>>';"""
            compare(f"{name}: imported rows", tinker(php, fake=False)["keys"], json.loads(rows))
            # What the model reads back from a row coolify-rs created.
            php = f"""
$a = \\App\\Models\\Application::find({app_id}); $e = $a->environment_variables()->orderBy('key')->first();
echo '<<<'.json_encode(['value' => $e->value, 'runtime' => (bool) $e->is_runtime, 'build' => (bool) $e->is_buildtime, 'preview_copies' => $a->environment_variables_preview()->count(), 'count' => $a->environment_variables()->count()]).'>>>';"""
            back = tinker(php, fake=False)
            record(f"{name}: reference reads the imported row", back["value"] == "{{vault." + sorted(rs["result_keys"], key=str)[0 if rs["result_keys"][0].isupper() else 0].replace("db-dash", "") + "}}" or back["value"].startswith("{{vault."), back)
            record(f"{name}: preview copies", back["preview_copies"] == back["count"], back)

    # Cloudflare token validator.
    for token, caps in [("cf-good", ["dns"]), ("cf-good", ["dns", "tunnel"]), ("cf-403", ["dns"]), ("cf-dnsonly", ["dns", "tunnel"]), ("nope", ["dns"])]:
        fake("/__reset", "POST")
        php = f"echo '<<<'.json_encode(['valid' => (new \\App\\Services\\CloudflareTokenValidator)->validate({json.dumps(token)}, {json.dumps(caps)})]).'>>>';"
        ref = tinker(php)
        ref_requests = fake_log()
        fake("/__reset", "POST")
        code, body = ui("POST", "/ui/security/integration-tokens", {"provider": "cloudflare", "name": f"diff-cf-{token}-{len(caps)}", "token": token, "capabilities": caps})
        if code == 200:
            created["integration_tokens"].append(int(sql(f"SELECT id FROM integration_tokens WHERE name = 'diff-cf-{token}-{len(caps)}' ORDER BY id DESC LIMIT 1")))
        compare(f"cloudflare validator {token} {caps}", ref, {"valid": code == 200})
        compare(f"cloudflare validator requests {token} {caps}", ref_requests, fake_log())

    # syncZones and every DNS operation on the same token row, one side after the other.
    tid = int(sql("SELECT id FROM integration_tokens WHERE name = 'diff-cf-cf-good-2' ORDER BY id DESC LIMIT 1"))
    uuid = sql(f"SELECT uuid FROM integration_tokens WHERE id = {tid}")
    zones_sql = f"SELECT coalesce(json_agg(json_build_object('zone', provider_zone_id, 'name', name, 'account_id', account_id, 'account_name', account_name) ORDER BY provider_zone_id), '[]') FROM dns_provider_zones WHERE integration_token_id = {tid}"
    fake("/__reset", "POST")
    php = f"$t = \\App\\Models\\IntegrationToken::find({tid}); echo '<<<'.json_encode(['count' => app(\\App\\Services\\Dns\\CloudflareDnsProvider::class)->syncZones($t)]).'>>>';"
    ref_sync = tinker(php)
    ref_zones = json.loads(sql(zones_sql))
    ref_requests = fake_log()
    sql(f"DELETE FROM dns_provider_zones WHERE integration_token_id = {tid}")
    fake("/__reset", "POST")
    code, body = ui("POST", f"/ui/security/integration-tokens/{uuid}/refresh-zones")
    compare("syncZones: message", f"Cloudflare zones refreshed. {ref_sync['count']} accessible zones found.", body.get("message"))
    compare("syncZones: rows", ref_zones, json.loads(sql(zones_sql)))
    compare("syncZones: requests", ref_requests, fake_log())
    compare("syncZones: metadata shape", True, sql(f"SELECT metadata::jsonb ? 'zones_synced_at' FROM integration_tokens WHERE id = {tid}") == "t")
    # A second token with no permission.
    r = ref_create_token("cloudflare", "diff-cf-denied", "cf-403", ["dns"], {})
    code, body = ui("POST", f"/ui/security/integration-tokens/{r['uuid']}/refresh-zones")
    php = f"$t = \\App\\Models\\IntegrationToken::find({r['id']}); try {{ app(\\App\\Services\\Dns\\CloudflareDnsProvider::class)->syncZones($t); $m = 'none'; }} catch (\\Throwable $e) {{ $m = $e->getMessage(); }} echo '<<<'.json_encode($m).'>>>';"
    compare("syncZones: 403", tinker(php), body.get("message"))

    tunnel_section(ui, tid)
    uncompromised_section(ui)

    same = sum(1 for _, s in results if s)
    print(f"\n{same}/{len(results)} same")
    return 0 if same == len(results) else 1


if __name__ == "__main__":
    import signal
    for sig in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, lambda *_: sys.exit(1))
    try:
        code = main()
    finally:
        cleanup()
    sys.exit(code)
