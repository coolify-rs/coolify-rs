#!/usr/bin/env python3
"""Differential cases of the application create fields, POST /applications/dockerfile and
PATCH /applications/{uuid}: upstream reference vs coolify-rs. Nothing is deployed.

Used by applications_public_diff.py (git types and PATCH) and applications_diff.py (Docker image
and Dockerfile types). Run alone with:

    COOLIFY_RS_BASE=http://127.0.0.1:18082 applications_fields_diff.py [--server <uuid>] [--project <uuid>]

Rows are compared on every accepted request: all columns of applications and application_settings
except ids and timestamps, encrypted columns by decrypted value, generated labels with the uuid and
password hashes masked, and config_hash against what upstream computes for the coolify-rs row.
Only rows this script created are deleted, tracked by uuid.
"""
import argparse
import base64
import difflib
import json
import os
import re
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from databases_diff import call as raw_call, record, results, sql  # noqa: E402

TOKEN = "acme_owner_root"
REPO = "https://github.com/coollabsio/coolify-examples"
SKIP = {"id", "uuid", "created_at", "updated_at", "last_online_at", "custom_labels", "config_hash",
        "manual_webhook_secret_github", "manual_webhook_secret_gitlab", "manual_webhook_secret_bitbucket",
        "manual_webhook_secret_gitea", "http_basic_auth_password"}
ENCRYPTED = ["manual_webhook_secret_github", "manual_webhook_secret_gitlab", "manual_webhook_secret_bitbucket",
             "manual_webhook_secret_gitea", "http_basic_auth_password"]
HASH = re.compile(r"\$2y\$\d\d\$[./A-Za-z0-9]{53}|\$argon2id\$[^\"\s]+")
COMPOSE = "services:\n  web:\n    image: nginx\n  api:\n    image: nginx\n"


def call(*args, **kwargs):
    """Other agents share the reference's rate limit: wait longer than `databases_diff.call` does."""
    for _ in range(10):
        try:
            return raw_call(*args, **kwargs)
        except SystemExit:
            time.sleep(20)
    raise SystemExit("rate limited")


def sided(body, side):
    """`@S@` in a request: unique per side, for values both sides must not share."""
    return json.loads(json.dumps(body).replace("@S@", "ra" if side == "ref" else "rb"))


VOLATILE = {"id", "uuid", "created_at", "updated_at", "last_online_at", "config_hash", "application_id", "server_id", "destination_id",
            "environment_id", "project_id", "manual_webhook_secret_github", "manual_webhook_secret_gitlab", "manual_webhook_secret_bitbucket",
            "manual_webhook_secret_gitea"}


def b64(s):
    return base64.b64encode(s.encode() if isinstance(s, str) else s).decode()


def php(code):
    """Runs PHP inside the bootstrapped reference application."""
    return subprocess.run(["docker", "exec", "coolify-rs-reference-coolify-1", "php", "-r",
                           "require '/var/www/html/vendor/autoload.php'; $app = require '/var/www/html/bootstrap/app.php'; "
                           "$app->make(Illuminate\\Contracts\\Console\\Kernel::class)->bootstrap(); " + code],
                          capture_output=True, text=True).stdout.strip()


def app_row(u):
    return json.loads(sql(f"SELECT row_to_json(x)::text FROM applications x WHERE uuid='{u}'") or "null")


def settings_row(app_id):
    r = json.loads(sql(f"SELECT row_to_json(s)::text FROM application_settings s WHERE application_id={app_id}") or "null")
    for k in ("id", "application_id", "created_at", "updated_at"):
        r and r.pop(k)
    return r


def secrets_and_hash(*uuids):
    """Decrypted columns and upstream's configuration hash of the rows, from one PHP run."""
    fields = ",".join(f"'{f}'" for f in ENCRYPTED)
    wanted = ",".join(f"'{u}'" for u in uuids)
    out = php("$out = []; foreach ([" + wanted + "] as $u) { $a = App\\Models\\Application::where('uuid', $u)->first(); $r = [];"
              " foreach ([" + fields + "] as $f) { $r[$f] = $a->$f; }"
              " $r['hash'] = $a->deploymentConfigurationHash(); $out[$u] = $r; } echo json_encode($out);")
    return json.loads(out)


class Cases:
    def __init__(self, base):
        self.base = base
        self.created = []
        self.names = []

    def post(self, side, path, body, tok=TOKEN):
        c, r = call(side, "POST", path, body, tok=tok)
        try:
            j = json.loads(r)
        except ValueError:
            j = None
        if c == 201 and isinstance(j, dict) and j.get("uuid"):
            self.created.append(j["uuid"])
        return c, r, j

    def same_error(self, name, path, body, tok=TOKEN):
        ra, rb = self.post("ref", path, body, tok), self.post("rs", path, body, tok)
        record(name, (ra[0], ra[1]) == (rb[0], rb[1]), f"ref {ra[0]} {ra[1][:900]}\n  rs  {rb[0]} {rb[1][:900]}")

    def rows(self, name, ua, ub, check_hash=True):
        ra, rb = app_row(ua), app_row(ub)
        for row, u in ((ra, ua), (rb, ub)):
            row["fqdn"] = row["fqdn"].replace(u, "U") if row["fqdn"] else row["fqdn"]
            for k in ("fqdn", "docker_compose_domains", "noindex_domains", "domain_port_overrides"):
                row[k] = re.sub(r"-r[ab]\b", "-S", json.dumps(row[k]))
        diff = {k: (ra[k], rb.get(k)) for k in ra if k not in SKIP and ra[k] != rb.get(k)}
        record(f"{name}: application rows alike", not diff, diff)
        sa, sb = settings_row(ra["id"]), settings_row(rb["id"])
        for k in ("custom_internal_name", "custom_container_name_prefix"):
            for row in (sa, sb):
                row[k] = re.sub(r"-r[ab]$", "", row[k]) if row[k] else row[k]
        record(f"{name}: settings rows alike", sa == sb, {k: (sa[k], sb.get(k)) for k in sa if sa[k] != sb.get(k)})
        la = re.sub(r"-r[ab]\b", "-S", HASH.sub("H", (ra["custom_labels"] or "").replace(ua, "U")))
        lb = re.sub(r"-r[ab]\b", "-S", HASH.sub("H", (rb["custom_labels"] or "").replace(ub, "U")))
        record(f"{name}: labels identical", la == lb, "\n".join(difflib.unified_diff(la.splitlines(), lb.splitlines(), lineterm="")))
        php_rows = secrets_and_hash(ua, ub)
        pa, pb = php_rows[ua], php_rows[ub]
        same_secrets = all(pa[f] == pb[f] or (len(pa[f] or "") == 40 and len(pb[f] or "") == 40) for f in ENCRYPTED)
        record(f"{name}: decrypted columns alike", same_secrets, {f: (pa[f], pb[f]) for f in ENCRYPTED if pa[f] != pb[f]})
        if check_hash:
            record(f"{name}: config_hash is what upstream computes for the coolify-rs row",
                   rb["config_hash"] == pb["hash"] and ra["config_hash"] == pa["hash"],
                   f"rs {rb['config_hash']} upstream {pb['hash']}")
        return ra, rb

    def pair(self, name, path, body):
        """Creates on both sides and compares status, shape and rows."""
        ra, rb = self.post("ref", path, sided(body, "ref")), self.post("rs", path, sided(body, "rs"))
        ok = ra[0] == rb[0] == 201 and list(ra[2] or {}) == list(rb[2] or {})
        record(f"{name}: create status and shape", ok, f"ref {ra[0]} {ra[1][:700]}\n  rs  {rb[0]} {rb[1][:700]}")
        if not ok:
            return None, None
        ua, ub = ra[2]["uuid"], rb[2]["uuid"]
        record(f"{name}: response domains", (ra[2]["domains"] or "").replace(ua, "U") == (rb[2]["domains"] or "").replace(ub, "U"),
               f"{ra[2]} / {rb[2]}")
        self.rows(name, ua, ub)
        return ua, ub

    def patch(self, name, pair, body, tok=TOKEN, check_rows=True):
        """The same PATCH on both sides: status, body with the uuid masked, rows afterwards."""
        ua, ub = pair
        ca, ra = call("ref", "PATCH", f"/api/v1/applications/{ua}", sided(body, "ref"), tok=tok)
        cb, rb = call("rs", "PATCH", f"/api/v1/applications/{ub}", sided(body, "rs"), tok=tok)
        ta, tb = ra.decode().replace(ua, "U"), rb.decode().replace(ub, "U")
        record(f"{name}: response", (ca, ta) == (cb, tb), f"ref {ca} {ta[:900]}\n  rs  {cb} {tb[:900]}")
        if check_rows:
            self.rows(name, ua, ub, check_hash=False)

    def show(self, name, pair):
        """GET /applications/{uuid} on both sides, ids and timestamps aside, with several tokens."""
        def clean(v, u):
            if isinstance(v, dict):
                return {k: clean(x, u) for k, x in v.items() if k not in VOLATILE}
            if isinstance(v, list):
                return [clean(x, u) for x in v]
            return re.sub(r"-r[ab]\b", "-S", HASH.sub("H", v.replace(u, "U"))) if isinstance(v, str) else v
        for tok in ("acme_owner_read", TOKEN):
            ca, ra = call("ref", "GET", f"/api/v1/applications/{pair[0]}", tok=tok)
            cb, rb = call("rs", "GET", f"/api/v1/applications/{pair[1]}", tok=tok)
            ja, jb = clean(json.loads(ra), pair[0]), clean(json.loads(rb), pair[1])
            keys = [k for k in dict.fromkeys([*ja, *jb]) if ja.get(k) != jb.get(k)]
            record(f"{name}: show [{tok}]", ca == cb and ja == jb and list(ja) == list(jb),
                   f"{ca} {cb} " + "; ".join(f"{k}: {ja.get(k)} | {jb.get(k)}" for k in keys)[:1500])

    def verify_hashes(self, name, u, password):
        """Every password hash in the labels of a row verifies with PHP's password_verify."""
        labels = app_row(u)["custom_labels"] or ""
        hashes = HASH.findall(labels)
        verdict = php("foreach (json_decode('" + json.dumps(hashes) + "') as $h) { echo password_verify('" + password + "', $h) ? 'y' : 'n'; }")
        record(f"{name}: label hashes verify in PHP", bool(hashes) and set(verdict) == {"y"}, f"{hashes} {verdict}")

    def track_by_name(self, name):
        """Rows a failed upstream request left behind (no uuid in its answer)."""
        for u in sql(f"SELECT uuid FROM applications WHERE name='{name}'").splitlines():
            if u and u not in self.created:
                self.created.append(u)

    def cleanup(self):
        for u in self.created:
            call("ref", "DELETE", f"/api/v1/applications/{u}", tok=TOKEN)
        time.sleep(10)
        for u in self.created:
            rid = sql(f"SELECT id FROM applications WHERE uuid='{u}'")
            if rid:
                sql(f"DELETE FROM environment_variables WHERE resourceable_type='App\\Models\\Application' AND resourceable_id={rid}")
                sql(f"DELETE FROM application_settings WHERE application_id={rid}")
                sql(f"DELETE FROM applications WHERE id={rid}")


def rich_fields(prefix):
    """Every field a create request can send that has no type-specific meaning."""
    return dict(
        name=f"{prefix}-rich", description="<b>d</b>",
        watch_paths="src/**\n!/docs/**\n/lib/x\n\n", custom_nginx_configuration=b64("server { listen 80; }\n"),
        use_build_server=True, connect_to_docker_network=True, use_build_secrets=True,
        is_http_basic_auth_enabled=True, http_basic_auth_username="admin", http_basic_auth_password="s3cret-passw0rd",
        http_basic_auth_hash_algorithm="bcrypt", http_basic_auth_bcrypt_cost=5,
        http_basic_auth_argon2id_memory_cost=8192, http_basic_auth_argon2id_time_cost=2,
        custom_docker_run_options="--cap-add=NET_ADMIN --ulimit nofile=1024",
        post_deployment_command="php artisan migrate && echo 'done'", post_deployment_command_container="app",
        pre_deployment_command="echo pre", pre_deployment_command_container="app",
        manual_webhook_secret_github="a" * 16, manual_webhook_secret_gitlab="b" * 20, manual_webhook_secret_bitbucket=None,
        manual_webhook_secret_gitea="c" * 40,
        is_gpu_enabled=True, gpu_driver="nvidia", gpu_count="all", gpu_device_ids="0,1", gpu_options="a=1\nb=2",
        is_consistent_container_name_enabled=True, custom_internal_name=f"{prefix}-internal-@S@",
        custom_container_name_prefix=f"{prefix} Pre_Fix-@S@", is_env_sorting_enabled=True,
        traefik_certificate_resolver="letsencrypt", is_permanent_redirect_enabled=True, is_log_drain_enabled=True,
        is_pr_deployments_public_enabled=True, docker_images_to_keep=4, stop_grace_period=30, rolling_update_drain_seconds=5,
        custom_network_aliases=" one two, three ,one two", max_restart_count=7, preview_url_template="{{pr_id}}-x.{{domain}}",
        docker_compose_raw=COMPOSE, git_commit_sha="abc1234", docker_registry_image_tag="v1", is_preserve_repository_enabled=True,
        limits_cpu_shares=512, health_check_return_code=204, redact_all_env_values_in_logs=True,
        include_coolify_default_variables=False, autogenerate_domain=False,
    )


def run_create_errors(c, path, ok):
    """Validation paths every create type shares. `ok` is a body the type accepts."""
    for name, extra in [
        ("watch_paths array throws", dict(watch_paths=["x"])),
        ("custom_nginx not base64", dict(custom_nginx_configuration="not base64!")),
        ("custom_nginx not utf-8", dict(custom_nginx_configuration=b64(b"\xff\xfe"))),
        ("custom_nginx number", dict(custom_nginx_configuration=5)),
        ("custom_nginx array throws", dict(custom_nginx_configuration=["x"])),
        ("custom_labels not base64", dict(custom_labels="not base64!")),
        ("custom_labels null", dict(custom_labels=None)),
        ("custom_labels not utf-8", dict(custom_labels=b64(b"\xff\xfe"))),
        ("custom_labels array throws", dict(custom_labels=["x"])),
        ("basic auth password too long", dict(http_basic_auth_password="x" * 73)),
        ("basic auth flag invalid", dict(is_http_basic_auth_enabled="maybe")),
        ("basic auth username type", dict(http_basic_auth_username=5)),
        ("hash algorithm unknown", dict(http_basic_auth_hash_algorithm="md5")),
        ("argon2id needs caddy", dict(http_basic_auth_hash_algorithm="argon2id")),
        ("bcrypt cost range", dict(http_basic_auth_bcrypt_cost=3)),
        ("argon memory range", dict(http_basic_auth_argon2id_memory_cost=1)),
        ("argon time range", dict(http_basic_auth_argon2id_time_cost=99)),
        ("run options unsafe", dict(custom_docker_run_options="a; rm -rf /")),
        ("pre container invalid", dict(pre_deployment_command_container="-bad name")),
        ("post container null", dict(post_deployment_command_container=None)),
        ("pre command type", dict(pre_deployment_command=5)),
        ("webhook short", dict(manual_webhook_secret_github="short")),
        ("webhook type", dict(manual_webhook_secret_gitlab=5)),
        ("gpu count invalid", dict(gpu_count="many")),
        ("gpu options invalid", dict(gpu_options="just text")),
        ("gpu options nested", dict(gpu_options="a:\n  b: 1\n")),
        ("gpu options array throws", dict(gpu_options=["x"])),
        ("gpu driver type", dict(gpu_driver=5)),
        ("internal name invalid", dict(custom_internal_name="-bad name")),
        ("internal name with naming off", dict(custom_internal_name="okname", is_consistent_container_name_enabled=False)),
        ("prefix too long", dict(custom_container_name_prefix="x" * 31)),
        ("prefix type array", dict(custom_container_name_prefix=["x"])),
        ("prefix number", dict(custom_container_name_prefix=5)),
        ("traefik resolver invalid", dict(traefik_certificate_resolver="a b")),
        ("env sorting type", dict(is_env_sorting_enabled="maybe")),
        ("images to keep range", dict(docker_images_to_keep=101)),
        ("noindex type", dict(noindex_domains="x")),
        ("noindex item type", dict(noindex_domains=["https://a.example.com", 5])),
        ("aliases type", dict(custom_network_aliases=5)),
        ("restart count negative", dict(max_restart_count=-1)),
        ("preview template null", dict(preview_url_template=None)),
        ("compose raw not string", dict(docker_compose_raw=5)),
        ("compose raw invalid yaml", dict(docker_compose_raw="a: [")),
        ("compose raw no services", dict(docker_compose_raw="version: '3'\n")),
        ("compose raw bad service name", dict(docker_compose_raw="services:\n  'a;b':\n    image: nginx\n")),
        ("compose raw bad volume", dict(docker_compose_raw="services:\n  a:\n    image: nginx\n    volumes:\n      - '$(id):/x'\n")),
        ("compose raw bad network", dict(docker_compose_raw="services:\n  a:\n    image: nginx\nnetworks:\n  'a;b': {}\n")),
        ("extra field", dict(not_a_field=1, also_not=2)),
        ("fqdn field", dict(fqdn="https://a.example.com")),
        ("compose domains type", dict(docker_compose_domains="x")),
        ("tag item short", dict(tags=["a"])),
    ]:
        c.same_error(f"create error: {name}", path, dict(ok, **extra))
    c.same_error("create error: webhook secret without deploy ability", path,
                 dict(ok, manual_webhook_secret_github="d" * 16), tok="acme_owner_write")
    c.same_error("create error: member cannot create", path, ok, tok="acme_member_write_legacy")


def run_git_create(c):
    """Fields, compose domains and their errors on the git create types."""
    base = c.base
    path = "/api/v1/applications/public"
    git = dict(base, git_repository=REPO, git_branch="main", build_pack="nixpacks", ports_exposes="3000")
    compose = dict(base, git_repository=REPO, git_branch="main", build_pack="dockercompose",
                   docker_compose_location="/missing/docker-compose.yml")
    rich = c.pair("public every field", path, dict(git, **rich_fields("p2f-pub")))
    if rich[0]:
        c.show("public every field", rich)
    basic = c.pair("public basic auth labels", path, dict(git, name="p2f-basic", domains="https://p2f-basic.example.com", force_domain_override=True,
                                                  is_http_basic_auth_enabled=True, http_basic_auth_username="u", http_basic_auth_password="p",
                                                  is_force_https_enabled=False))
    if basic[1]:
        c.verify_hashes("public basic auth labels", basic[1], "p")
    c.pair("public basic auth forced https", path, dict(git, name="p2f-basic2", domains="https://p2f-basic.example.com,http://p2f-b2.example.com", force_domain_override=True,
                                                        is_http_basic_auth_enabled=True, http_basic_auth_username="u", http_basic_auth_password="p"))
    c.pair("public basic auth without password", path, dict(git, name="p2f-basic3", is_http_basic_auth_enabled=True, http_basic_auth_username="u",
                                                            http_basic_auth_password=None, autogenerate_domain=False))
    c.pair("public custom labels kept without readonly", path, dict(git, name="p2f-labels", is_container_label_readonly_enabled=False,
                                                                    custom_labels=b64(b64("a=1\nb=2")), autogenerate_domain=False))
    c.pair("public custom labels replaced with readonly", path, dict(git, name="p2f-labels2", custom_labels=b64("a=1"), autogenerate_domain=False))
    c.pair("public noindex kept", path, dict(git, name="p2f-noindex", domains="https://p2f-no.example.com:8443,https://p2f-no2.example.com", force_domain_override=True,
                                             noindex_domains=["HTTPS://P2F-NO.example.com", "https://other.example.com", "https://p2f-no.example.com"]))
    c.pair("public noindex empty list", path, dict(git, name="p2f-noindex2", noindex_domains=[], autogenerate_domain=False))
    c.pair("public noindex without domains", path, dict(git, name="p2f-noindex3", noindex_domains=["https://x.example.com"], autogenerate_domain=False))
    c.pair("public noindex null", path, dict(git, name="p2f-noindex4", noindex_domains=None, autogenerate_domain=False))
    c.pair("public aliases json", path, dict(git, name="p2f-alias", custom_network_aliases='["a b","a b","c"]', autogenerate_domain=False))
    c.pair("public aliases null", path, dict(git, name="p2f-alias2", custom_network_aliases=None, autogenerate_domain=False))
    c.pair("public connect flag strings", path, dict(git, name="p2f-flag", connect_to_docker_network="yes", use_build_server="off", autogenerate_domain=False))
    c.pair("public settings off", path, dict(git, name="p2f-off", is_gzip_enabled=False, is_stripprefix_enabled=False, is_container_label_escape_enabled=False,
                                             is_container_label_readonly_enabled=False, domains="https://p2f-off.example.com/a", force_domain_override=True))
    c.pair("public internal name alone turns naming on", path, dict(git, name="p2f-internal", custom_internal_name="p2f-only-name-@S@", autogenerate_domain=False))
    c.pair("public prefix slugged to nothing", path, dict(git, name="p2f-pre0", custom_container_name_prefix="!!!", autogenerate_domain=False))
    c.pair("public empty optional fields", path, dict(git, name="p2f-empty", watch_paths="", custom_nginx_configuration=None, description=None, http_basic_auth_username=None,
                                                      gpu_count=None, gpu_options=None, custom_internal_name=None, autogenerate_domain=False))
    c.pair("public watch paths number", path, dict(git, name="p2f-watch", watch_paths=5, autogenerate_domain=False))
    c.pair("public dockerfile raw value", path, dict(git, name="p2f-rawdf", dockerfile="FROM nginx", autogenerate_domain=False))
    c.pair("public compose domains on nixpacks", path, dict(git, name="p2f-cdn", autogenerate_domain=False,
                                                            docker_compose_domains=[{"name": "web", "domain": "https://p2f-cdn.example.com:81"}]))

    # A prefix in use.
    ua, ub = c.pair("public prefix taken", path, dict(git, name="p2f-pre1", custom_container_name_prefix="p2f-taken-@S@", autogenerate_domain=False))
    if ua:
        c.same_error("prefix in use", path, dict(git, name="p2f-pre2", custom_container_name_prefix="p2f-taken-ra", autogenerate_domain=False))
        c.same_error("prefix equal to a uuid", path, dict(git, name="p2f-pre3", custom_container_name_prefix=ua, autogenerate_domain=False))
    c.pair("public internal name taken", path, dict(git, name="p2f-pre4", custom_internal_name="p2f-taken2-@S@", autogenerate_domain=False))
    c.same_error("prefix equal to an internal name", path, dict(git, name="p2f-pre5", custom_container_name_prefix="p2f-taken2-ra", autogenerate_domain=False))

    # Compose domains.
    c.pair("compose domains", path, dict(compose, name="p2f-c1", docker_compose_domains=[
        {"name": "web", "domain": "https://p2f-web.example.com:8080,https://www.p2f-web.example.com", "redirect": "www"},
        {"name": "api", "domain": "http://p2f-api.example.com/x", "redirect": None},
        {"name": "db"},
    ], force_domain_override=True, noindex_domains=["https://p2f-web.example.com", "https://not-here.example.com"]))
    c.pair("compose domains by keys", path, dict(compose, name="p2f-c2", force_domain_override=True,
                                                 docker_compose_domains={"x": {"name": "web", "domain": "https://p2f-web2.example.com"}}))
    c.pair("compose domains null", path, dict(compose, name="p2f-c3", docker_compose_domains=None))
    c.pair("compose domains conflict forced", path, dict(compose, name="p2f-c4", force_domain_override=True,
                                                         docker_compose_domains=[{"name": "web", "domain": "https://p2f-web.example.com"},
                                                                                 {"name": "api", "domain": "https://p2f-web.example.com"}]))
    # A conflict against rows both sides see: only the reference's row exists yet for this domain.
    for name, doms in [
        ("conflict with fqdn", [{"name": "web", "domain": "https://p2f-basic.example.com"}]),
        ("conflict with compose service", [{"name": "web", "domain": "http://p2f-api.example.com/x"}]),
        ("duplicate urls", [{"name": "web", "domain": "https://p2f-dup.example.com"}, {"name": "api", "domain": "https://p2f-dup.example.com"}]),
        ("invalid url", [{"name": "web", "domain": "ftp://x.example.com"}]),
        ("no fqdn host", [{"name": "web", "domain": "https://localhost"}]),
        ("item not array", ["x"]),
        ("item extra key", [{"name": "web", "domain": "https://a.example.com", "x": 1}]),
        ("item list", [["a"]]),
        ("item null", [None]),
        ("name missing", [{"domain": "https://a.example.com"}]),
        ("name type", [{"name": 5}]),
        ("domain type", [{"name": "web", "domain": 5}]),
        ("domain too long", [{"name": "web", "domain": "https://" + "a" * 2050 + ".example.com"}]),
        ("redirect invalid", [{"name": "web", "redirect": "x"}]),
        ("redirect type", [{"name": "web", "redirect": 5}]),
        ("empty list throws", []),
    ]:
        c.same_error(f"compose domains: {name}", path, dict(compose, name=f"p2f-err-{name[:8]}", docker_compose_domains=doms))
        c.track_by_name(f"p2f-err-{name[:8]}")

    # The domain conflict answer against an application and with `domains`.
    c.same_error("domains conflict answer", path, dict(git, name="p2f-confl", domains="https://p2f-basic.example.com/"))
    c.same_error("domains conflict with compose service", path, dict(git, name="p2f-confl", domains="http://p2f-api.example.com/x"))

    # Flags without a rule that the database rejects: upstream leaves a row behind (known gap).
    c.same_error("connect flag postgres rejects", path, dict(git, name="p2f-badflag", connect_to_docker_network="maybe", autogenerate_domain=False))
    c.track_by_name("p2f-badflag")

    run_create_errors(c, path, dict(git, name="p2f-e", autogenerate_domain=False))

    # The private types run the same block with their own duplicate message.
    c.same_error("deploy key: compose domains validated", "/api/v1/applications/private-deploy-key",
                 dict(compose, name="p2f-dk", private_key_uuid="nope", docker_compose_domains=["x"]))


def run_image_create(c):
    """The extra create fields on dockerimage, and POST /applications/dockerfile."""
    base = c.base
    image = "/api/v1/applications/dockerimage"
    df = "/api/v1/applications/dockerfile"
    img = dict(base, docker_registry_image_name="nginx", ports_exposes="80")
    c.pair("dockerimage every field", image, dict(img, **rich_fields("p2f-img"), is_preview_deployments_enabled=True, is_static=True, is_spa=True,
                                                  install_command="npm ci", build_command=None, base_directory="/x/", publish_directory=None,
                                                  dockerfile_location=None, docker_compose_location="/c.yml/", dockerfile_target_build="prod",
                                                  git_repository="https://ignored.example.com/x", git_branch="ignored", build_pack="static"))
    c.pair("dockerimage dockerfile location from request pack", image, dict(img, name="p2f-img2", build_pack="dockerfile", dockerfile_location=None, autogenerate_domain=False))
    c.same_error("dockerimage compose domains throw", image, dict(img, name="p2f-img3", docker_compose_domains=[{"name": "web"}]))
    c.track_by_name("p2f-img3")
    run_create_errors(c, image, dict(img, name="p2f-e", autogenerate_domain=False))

    dockerfile = "FROM nginx:alpine\nEXPOSE 8080\nHEALTHCHECK --interval=30s CMD true\n"
    ok = dict(base, dockerfile=b64(dockerfile))
    c.pair("dockerfile minimal named", df, dict(ok, name="p2f-df1", autogenerate_domain=False))
    c.pair("dockerfile generated domain", df, dict(ok, name="p2f-df2"))
    c.pair("dockerfile every field", df, dict(ok, **rich_fields("p2f-df"), is_preview_deployments_enabled=True, ports_exposes="9999", build_pack="static",
                                              dockerfile_location="/x/Dockerfile/", git_repository="x", domains="https://p2f-df.example.com:3000", force_domain_override=True))
    # Upstream reads the port from the encoded text; base64 starting with EXPOSE never decodes to UTF-8.
    c.same_error("dockerfile base64 starting with EXPOSE", df, dict(base, name="p2f-df3", dockerfile="EXPOSE12", autogenerate_domain=False))
    for name, body in [
        ("missing", dict(base)),
        ("not string", dict(base, dockerfile=5)),
        ("null", dict(base, dockerfile=None)),
        ("not base64", dict(base, dockerfile="not base64!")),
        ("not utf-8", dict(base, dockerfile=b64(b"\xff\xfe"))),
        ("project missing", dict(ok, project_uuid="nope")),
        ("server missing", dict(ok, server_uuid="nope")),
        ("environment missing", dict(ok, environment_name="nope")),
        ("no environment", {"project_uuid": base["project_uuid"], "server_uuid": base["server_uuid"], "dockerfile": b64("FROM x")}),
        ("raw compose flag", dict(ok, is_raw_compose_deployment_enabled=True)),
        ("bad domain", dict(ok, domains="ftp://a.example.com")),
        ("ports mappings duplicate", dict(ok, ports_mappings="80:80,80:81")),
        ("instant deploy without deploy ability", dict(ok, instant_deploy=True)),
    ]:
        tok = "acme_owner_write" if name.startswith("instant") else TOKEN
        c.same_error(f"dockerfile error: {name}", df, body, tok=tok)
    c.same_error("dockerfile compose domains throw", df, dict(ok, name="p2f-df4", docker_compose_domains=[{"name": "web"}]))
    c.track_by_name("p2f-df4")
    run_create_errors(c, df, dict(ok, name="p2f-e", autogenerate_domain=False))


def run_update(c):
    """PATCH /applications/{uuid} on every kind of application."""
    base = c.base
    git = dict(base, git_repository=REPO, git_branch="main", build_pack="nixpacks", ports_exposes="3000")
    img = dict(base, docker_registry_image_name="nginx", ports_exposes="80")
    pub = c.pair("update fixture public", "/api/v1/applications/public",
                 dict(git, name="p2f-u-pub", domains="https://p2f-u1.example.com", force_domain_override=True))
    image = c.pair("update fixture image", "/api/v1/applications/dockerimage",
                   dict(img, name="p2f-u-img", domains="https://p2f-u2.example.com", force_domain_override=True))
    dockerfile = c.pair("update fixture dockerfile", "/api/v1/applications/dockerfile",
                        dict(base, name="p2f-u-df", dockerfile=b64("FROM nginx\n"), autogenerate_domain=False))
    compose = c.pair("update fixture compose", "/api/v1/applications/public",
                     dict(base, name="p2f-u-c", git_repository=REPO, git_branch="main", build_pack="dockercompose",
                          docker_compose_location="/missing/docker-compose.yml"))
    if not (pub[0] and image[0] and dockerfile[0] and compose[0]):
        return
    time.sleep(3)  # LoadComposeFile on the reference finds nothing and leaves the row.
    p = pub
    # Plain columns.
    c.patch("name and description", p, dict(name="p2f-u-<b>renamed</b>", description="<i>x</i>"))
    c.patch("description null", p, dict(description=None))
    c.patch("name only settings", p, dict(is_spa=True, is_auto_deploy_enabled=False))
    c.patch("every plain field", p, dict(
        git_repository=" https://gitlab.com/o/r.git ", git_branch="dev", git_commit_sha="abc", docker_registry_image_name="x/y",
        docker_registry_image_tag="v2", static_image="nginx:alpine", install_command=" npm ci ", build_command=None, start_command="npm start",
        ports_mappings="80:80,81:81", base_directory="base/", publish_directory="dist/", health_check_enabled=True, health_check_type="cmd",
        health_check_command="curl -f http://x", health_check_path="/up", health_check_port=8080, health_check_host="127.0.0.1", health_check_method="HEAD",
        health_check_return_code="204", health_check_scheme="https", health_check_response_text="ok", health_check_interval=10, health_check_timeout="11",
        health_check_retries=12, health_check_start_period=13, limits_memory="1g", limits_memory_swap="2g", limits_memory_swappiness=30, limits_memory_reservation="500m",
        limits_cpus="0.5", limits_cpuset="0-1", limits_cpu_shares=256, custom_docker_run_options="--init", post_deployment_command="post", post_deployment_command_container="c1",
        pre_deployment_command="pre", pre_deployment_command_container="c2", watch_paths="a/**\n!/b", dockerfile_location="/D/", dockerfile_target_build="t",
        docker_compose_location="c.yml", docker_compose_custom_start_command="up", docker_compose_custom_build_command="build", redirect="non-www",
        use_build_server=True, use_build_secrets=True, custom_network_aliases="x y,z", custom_nginx_configuration=b64("a"), preview_url_template="{{pr_id}}.x", max_restart_count=3))
    c.patch("nulls back", p, dict(install_command=None, build_command=None, start_command=None, publish_directory=None, ports_mappings=None, health_check_command=None,
                                  health_check_port=None, health_check_response_text=None, limits_cpuset=None, custom_docker_run_options=None, post_deployment_command=None,
                                  pre_deployment_command=None, watch_paths=None, dockerfile_location=None, docker_compose_location=None, dockerfile_target_build=None,
                                  docker_compose_custom_start_command=None, custom_network_aliases=None, custom_nginx_configuration=None, base_directory=None))
    c.patch("every setting", p, {k: v for k, v in rich_fields("p2f-up").items() if k in (
        "is_gpu_enabled", "gpu_driver", "gpu_count", "gpu_device_ids", "gpu_options", "is_consistent_container_name_enabled",
        "custom_internal_name", "custom_container_name_prefix", "is_env_sorting_enabled", "traefik_certificate_resolver", "is_permanent_redirect_enabled", "is_log_drain_enabled",
        "is_pr_deployments_public_enabled", "docker_images_to_keep", "stop_grace_period", "rolling_update_drain_seconds", "redact_all_env_values_in_logs",
        "include_coolify_default_variables")} | dict(is_git_submodules_enabled=False, is_git_lfs_enabled=False, is_git_shallow_clone_enabled=False, disable_build_cache=True,
                                                     inject_build_args_to_dockerfile=False, include_source_commit_in_build=True, is_preserve_repository_enabled=True,
                                                     is_preview_deployments_enabled=True, connect_to_docker_network=True, is_force_https_enabled=False, is_gzip_enabled=False,
                                                     is_stripprefix_enabled=False, is_container_label_escape_enabled=False))
    c.patch("settings back", p, dict(is_gzip_enabled=True, is_stripprefix_enabled=True, traefik_certificate_resolver=None, gpu_count=None, gpu_options=None, custom_internal_name=None,
                                     custom_container_name_prefix=None, stop_grace_period=None, is_permanent_redirect_enabled=False))
    # Domains, labels.
    c.patch("domains with a port", p, dict(domains="HTTPS://p2f-u1.example.com:8443,https://www.p2f-u1.example.com", force_domain_override=True))
    c.patch("domains same value again", p, dict(domains="https://p2f-u1.example.com,https://www.p2f-u1.example.com", force_domain_override=True))
    c.patch("noindex", p, dict(noindex_domains=["https://P2F-U1.example.com:8443", "https://not-mine.example.com"]))
    c.patch("noindex dropped with its domain", p, dict(domains="https://www.p2f-u1.example.com", force_domain_override=True))
    c.patch("noindex null", p, dict(noindex_domains=None))
    c.patch("domains null", p, dict(domains=None))
    c.patch("domains back", p, dict(domains="https://p2f-u1.example.com/", force_domain_override=True))
    c.patch("ports change regenerates labels", p, dict(ports_exposes="3000,4000"))
    c.patch("ports same keeps labels", p, dict(ports_exposes="3000,4000"))
    c.patch("is_static moves ports", p, dict(is_static=True))
    c.patch("is_static with the stale port", p, dict(ports_exposes="3000,4000", is_static=True))
    c.patch("is_static with a new port", p, dict(ports_exposes="5000", is_static=True))
    c.patch("is_static off", p, dict(is_static=False))
    c.patch("readonly off keeps labels", p, dict(is_container_label_readonly_enabled=False))
    c.patch("custom labels stored", p, dict(custom_labels=b64(b64("x=1\ny=2"))))
    c.patch("readonly on regenerates", p, dict(is_container_label_readonly_enabled=True))
    c.patch("custom labels with readonly", p, dict(custom_labels=b64("z=1")))
    c.patch("redirect and force https do not regenerate", p, dict(redirect="www", is_force_https_enabled=True))
    # Basic auth.
    c.patch("basic auth requires credentials", p, dict(is_http_basic_auth_enabled=True))
    c.patch("basic auth requires password", p, dict(is_http_basic_auth_enabled=True, http_basic_auth_username="u"))
    c.patch("basic auth on", p, dict(is_http_basic_auth_enabled=True, http_basic_auth_username="u", http_basic_auth_password="pass-word", http_basic_auth_bcrypt_cost=4))
    c.patch("basic auth stays with credentials", p, dict(is_http_basic_auth_enabled=True))
    c.patch("basic auth same password", p, dict(http_basic_auth_password="pass-word"))
    c.patch("basic auth off", p, dict(is_http_basic_auth_enabled=False))
    c.patch("basic auth null flag throws", p, dict(is_http_basic_auth_enabled=None))
    c.patch("argon2id needs caddy", p, dict(http_basic_auth_hash_algorithm="argon2id"))
    c.patch("bcrypt algorithm", p, dict(http_basic_auth_hash_algorithm="bcrypt"))
    # Webhook secrets.
    secret = "s" * 20
    c.patch("webhook set", p, dict(manual_webhook_secret_github=secret))
    c.patch("webhook same value without deploy", p, dict(manual_webhook_secret_github=secret), tok="acme_owner_write")
    c.patch("webhook change without deploy", p, dict(manual_webhook_secret_github="t" * 20), tok="acme_owner_write")
    c.patch("webhook clear without deploy", p, dict(manual_webhook_secret_gitea=None), tok="acme_owner_write")
    c.patch("webhook too short", p, dict(manual_webhook_secret_gitlab="short"))
    c.patch("webhook same short value accepted", p, dict(manual_webhook_secret_github=secret))
    c.patch("webhook type", p, dict(manual_webhook_secret_gitlab=5))
    # Errors.
    for name, body in [
        ("ports not numeric", dict(ports_exposes="80a")),
        ("ports out of range", dict(ports_exposes="70000")),
        ("ports zero", dict(ports_exposes="0")),
        ("ports null", dict(ports_exposes=None)),
        ("ports number", dict(ports_exposes=80)),
        ("ports array throws", dict(ports_exposes=["80"])),
        ("nginx not base64", dict(custom_nginx_configuration="not base64!")),
        ("nginx array throws", dict(custom_nginx_configuration=["x"])),
        ("nginx not utf-8", dict(custom_nginx_configuration=b64(b"\xff\xfe"))),
        ("dockerfile on another type", dict(dockerfile=b64("FROM x"))),
        ("labels not base64", dict(custom_labels="not base64!")),
        ("labels null", dict(custom_labels=None)),
        ("mappings duplicate", dict(ports_mappings="80:80,80:81")),
        ("mappings array throws", dict(ports_mappings=["x"])),
        ("domains type", dict(domains=5)),
        ("domains array throws", dict(domains=["https://a.example.com"])),
        ("domains invalid", dict(domains="ftp://a.example.com")),
        ("domains no fqdn", dict(domains="https://localhost")),
        ("domains conflict", dict(domains="https://p2f-u2.example.com")),
        ("extra fields", dict(project_uuid="x", tags=["a"], docker_compose_raw=COMPOSE, nope=1)),
        ("fqdn field", dict(fqdn="https://a.example.com")),
        ("name type", dict(name=5)),
        ("name too long", dict(name="x" * 256)),
        ("name null", dict(name=None)),
        ("git branch invalid", dict(git_branch="a b")),
        ("git branch null", dict(git_branch=None)),
        ("build pack invalid", dict(build_pack="dockerimage")),
        ("redirect invalid", dict(redirect="x")),
        ("static image invalid", dict(static_image="apache")),
        ("run options unsafe", dict(custom_docker_run_options="a; b")),
        ("watch paths type", dict(watch_paths=5)),
        ("username null", dict(http_basic_auth_username=None)),
        ("password null", dict(http_basic_auth_password=None)),
        ("password too long", dict(http_basic_auth_password="x" * 73)),
        ("flag invalid", dict(is_http_basic_auth_enabled="maybe")),
        ("raw compose on nixpacks", dict(is_raw_compose_deployment_enabled=True)),
        ("gpu options invalid", dict(gpu_options="text")),
        ("gpu options array throws", dict(gpu_options=["x"])),
        ("gpu driver null throws", dict(gpu_driver=None)),
        ("internal name with naming off", dict(custom_internal_name="abc", is_consistent_container_name_enabled=False)),
        ("prefix too long", dict(custom_container_name_prefix="x" * 31)),
        ("prefix array", dict(custom_container_name_prefix=["x"])),
        ("compose domains without raw", dict(docker_compose_domains=[{"name": "web"}])),
        ("compose domains item invalid", dict(docker_compose_domains=["x"])),
        ("github app missing", dict(github_app_uuid="nope")),
        ("github app type", dict(github_app_uuid=5)),
        ("several errors at once", dict(name=5, redirect="x", nope=1, gpu_count="x")),
    ]:
        c.patch(f"error: {name}", p, body)
    # A flag without a rule: its cast decides whether PostgreSQL sees it.
    c.patch("connect flag off", p, dict(connect_to_docker_network=False))
    c.patch("connect flag postgres rejects", p, dict(connect_to_docker_network="maybe"))
    c.patch("connect flag on", p, dict(connect_to_docker_network=True))
    c.patch("connect flag truthy text is not dirty", p, dict(connect_to_docker_network="off"))
    c.patch("connect flag text for a dirty value", p, dict(connect_to_docker_network=0))
    c.patch("connect flag postgres text", p, dict(connect_to_docker_network="yes"))
    # Requests and tokens.
    for name, body, tok in [
        ("member cannot update", dict(name="x"), "acme_member_write_legacy"),
        ("read token", dict(name="x"), "acme_owner_read"),
        ("instant deploy without deploy", dict(name="x", instant_deploy=True), "acme_owner_write"),
        ("write token accepted", dict(limits_memory="300m"), "acme_owner_write"),
        ("other team", dict(name="x"), "carol_root"),
    ]:
        c.patch(f"token: {name}", p, body, tok=tok)
    for name, r_path, body in [
        ("missing application", "/api/v1/applications/nope", dict(name="x")),
    ]:
        ra = call("ref", "PATCH", r_path, body, tok=TOKEN)
        rb = call("rs", "PATCH", r_path, body, tok=TOKEN)
        record(f"{name}", ra == rb, f"{ra} / {rb}")
    ra = call("ref", "PATCH", f"/api/v1/applications/{p[0]}", {}, tok=TOKEN)
    rb = call("rs", "PATCH", f"/api/v1/applications/{p[1]}", {}, tok=TOKEN)
    record("empty body", ra == rb, f"{ra} / {rb}")
    # The GitHub App switch: the public source (id 0) of the fixture team.
    public_app = sql("SELECT uuid FROM github_apps WHERE id=0")
    c.patch("github app public source", p, dict(github_app_uuid=public_app))
    c.patch("github app on image type", image, dict(github_app_uuid=public_app))
    c.patch("github app on dockerfile type", dockerfile, dict(github_app_uuid=public_app))
    c.show("after the updates", p)
    c.show("after the updates, dockerfile", dockerfile)

    # Build pack changes.
    c.patch("build pack to static", p, dict(build_pack="static"))
    c.patch("build pack to dockerfile with location", p, dict(build_pack="dockerfile", dockerfile_location=None))
    c.patch("build pack away from dockerfile clears", p, dict(build_pack="nixpacks", dockerfile_target_build="x"))

    # Docker image type.
    c.patch("image name not parsed", image, dict(docker_registry_image_name="ghcr.io/x/y", docker_registry_image_tag="v3"))
    c.patch("image name invalid", image, dict(docker_registry_image_name="NGINX"))

    # Dockerfile.
    d = dockerfile
    c.patch("dockerfile new content", d, dict(dockerfile=b64("FROM nginx:alpine\nEXPOSE 81\n")))
    c.patch("dockerfile not base64", d, dict(dockerfile="not base64!"))
    c.patch("dockerfile type", d, dict(dockerfile=5))
    c.patch("dockerfile blank", d, dict(dockerfile=b64("   ")))
    c.patch("dockerfile not utf-8", d, dict(dockerfile=b64(b"\xff\xfe")))
    c.patch("health check off", d, dict(health_check_enabled=False))
    c.patch("dockerfile healthcheck read", d, dict(dockerfile=b64("FROM x\nHEALTHCHECK --interval=30s --timeout=500ms \\\n  --start-period=2m --retries=7 CMD true\n")))
    c.patch("dockerfile healthcheck removed", d, dict(dockerfile=b64("FROM x\n")))
    c.patch("health check on", d, dict(health_check_enabled=True))
    c.patch("dockerfile healthcheck ignored while enabled", d, dict(dockerfile=b64("FROM y\nHEALTHCHECK --interval=9s CMD true\n")))
    c.patch("build pack away from dockerfile", d, dict(build_pack="nixpacks"))

    # Compose.
    k = compose
    for u in k:
        sql("UPDATE applications SET docker_compose_raw = $$" + COMPOSE + "$$ WHERE uuid='" + u + "'")
    c.patch("compose domains", k, dict(docker_compose_domains=[
        {"name": "web", "domain": "https://p2f-kw.example.com:8080,https://www.p2f-kw.example.com", "redirect": "www"},
        {"name": "api", "domain": "https://p2f-ka.example.com"},
        {"name": "not-a-service", "domain": "https://p2f-kn.example.com"},
    ], force_domain_override=True))
    c.patch("compose domains keep redirect", k, dict(docker_compose_domains=[{"name": "web", "domain": "https://p2f-kw.example.com"}], force_domain_override=True))
    c.patch("compose domains clear redirect", k, dict(docker_compose_domains=[{"name": "web", "domain": "https://p2f-kw.example.com", "redirect": None}], force_domain_override=True))
    c.patch("compose domains unknown only", k, dict(docker_compose_domains=[{"name": "nope", "domain": "https://p2f-kz.example.com"}]))
    c.patch("compose domains null", k, dict(docker_compose_domains=None))
    c.patch("compose domains duplicate", k, dict(docker_compose_domains=[{"name": "web", "domain": "https://d.example.com"}, {"name": "api", "domain": "https://d.example.com"}]))
    c.patch("compose domains invalid", k, dict(docker_compose_domains=[{"name": "web", "domain": "ftp://d.example.com"}]))
    c.patch("compose domains on a domain per side", k, dict(docker_compose_domains=[{"name": "api", "domain": "https://p2f-ks-@S@.example.com"}], force_domain_override=True))
    c.patch("compose domains conflict with itself ignored", k, dict(docker_compose_domains=[{"name": "api", "domain": "https://p2f-ks-@S@.example.com"}]))
    c.patch("compose domains conflict", k, dict(docker_compose_domains=[{"name": "api", "domain": "https://p2f-u1.example.com"}]))
    c.patch("compose domains noindex", k, dict(noindex_domains=["https://p2f-kw.example.com"]))
    c.patch("compose domains field", k, dict(domains="https://x.example.com"))
    c.patch("raw flag allowed", k, dict(is_raw_compose_deployment_enabled=True))
    c.patch("raw flag off", k, dict(is_raw_compose_deployment_enabled=False))
    # Service variables go when the build pack leaves dockercompose.
    for u in k:
        rid = sql(f"SELECT id FROM applications WHERE uuid='{u}'")
        sql("INSERT INTO environment_variables (uuid, key, value, is_preview, resourceable_type, resourceable_id, created_at, updated_at) VALUES "
            f"('p2f{u[:20]}', 'SERVICE_URL_WEB', 'x', false, 'App\\Models\\Application', {rid}, now(), now())")
    c.patch("build pack away from compose", k, dict(build_pack="nixpacks"))
    counts = [sql(f"SELECT count(*) FROM environment_variables WHERE resourceable_type='App\\Models\\Application' AND resourceable_id=(SELECT id FROM applications WHERE uuid='{u}') AND key='SERVICE_URL_WEB'") for u in k]
    record("service variables deleted on both sides", counts == ["0", "0"], counts)


def run_caddy(c):
    """Basic auth labels and argon2id on Caddy servers. The servers are only rows: nothing connects."""
    d = tempfile.mkdtemp()
    subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "diff", "-f", d + "/k"], check=True)
    ck, rk = call("ref", "POST", "/api/v1/security/keys", {"name": "p2f-caddy", "private_key": open(d + "/k").read()}, tok=TOKEN)
    key = json.loads(rk).get("uuid")
    record("caddy: key created", ck == 201 and bool(key), rk)
    servers = []
    try:
        # The deploy key type runs the same blocks; its duplicate message ends with a space.
        dk = "/api/v1/applications/private-deploy-key"
        keyed = dict(c.base, private_key_uuid=key, git_repository="git@github.com:o/r.git", git_branch="main")
        pair = c.pair("deploy key every field", dk, dict(keyed, build_pack="nixpacks", ports_exposes="3000", **rich_fields("p2f-dk")))
        if pair[0]:
            c.patch("deploy key: github app refused", pair, dict(github_app_uuid="nope"))
        c.pair("deploy key compose domains", dk, dict(keyed, name="p2f-dk2", build_pack="dockercompose", force_domain_override=True,
                                                      docker_compose_domains=[{"name": "web", "domain": "https://p2f-dk.example.com:81"}]))
        c.same_error("deploy key: duplicate urls", dk, dict(keyed, name="p2f-dk3", build_pack="dockercompose", docker_compose_domains=[
            {"name": "web", "domain": "https://p2f-dk-dup.example.com"}, {"name": "api", "domain": "https://p2f-dk-dup.example.com"}]))
        for label, image, exact in [("211", "lucaslorentz/caddy-docker-proxy:2.11-alpine", False),
                                    ("29", "lucaslorentz/caddy-docker-proxy:2.9-alpine", True),
                                    ("28", "lucaslorentz/caddy-docker-proxy:2.8-alpine", False)]:
            cs, rs = call("ref", "POST", "/api/v1/servers", {
                "name": f"p2f-caddy-{label}", "ip": f"198.51.100.{len(servers) + 10}", "port": 22, "user": "root",
                "private_key_uuid": key, "proxy_type": "caddy", "instant_validate": False}, tok=TOKEN)
            uuid = json.loads(rs).get("uuid")
            record(f"caddy {label}: server created", cs == 201 and bool(uuid), rs)
            servers.append(uuid)
            proxy = json.dumps({"type": "CADDY", "status": "exited",
                                "last_saved_proxy_configuration": f"services:\n  caddy:\n    image: {image}\n"})
            sql(f"UPDATE servers SET proxy = '{proxy}' WHERE uuid='{uuid}'")
            if exact:
                sql(f"UPDATE server_settings SET generate_exact_labels = true WHERE server_id=(SELECT id FROM servers WHERE uuid='{uuid}')")
            base = dict(c.base, server_uuid=uuid, git_repository=REPO, git_branch="main", build_pack="nixpacks", ports_exposes="3000",
                        domains=f"https://p2f-caddy-{label}.example.com", force_domain_override=True,
                        is_http_basic_auth_enabled=True, http_basic_auth_username="u", http_basic_auth_password="p")
            path = "/api/v1/applications/public"
            pair = c.pair(f"caddy {label}: bcrypt", path, dict(base, name=f"p2f-cb-{label}"))
            if pair[1]:
                c.verify_hashes(f"caddy {label}: bcrypt", pair[1], "p")
                c.patch(f"caddy {label}: patch to argon2id", pair, dict(http_basic_auth_hash_algorithm="argon2id", http_basic_auth_argon2id_memory_cost=8192,
                                                                        http_basic_auth_argon2id_time_cost=1))
                c.verify_hashes(f"caddy {label}: after the patch", pair[1], "p")
            if label == "211":
                pair = c.pair("caddy 211: argon2id", path, dict(base, name="p2f-ca-211", http_basic_auth_hash_algorithm="argon2id",
                                                                http_basic_auth_argon2id_memory_cost=8192, http_basic_auth_argon2id_time_cost=1))
                if pair[1]:
                    c.verify_hashes("caddy 211: argon2id", pair[1], "p")
            else:
                c.same_error(f"caddy {label}: argon2id refused", path, dict(base, name="p2f-cx", http_basic_auth_hash_algorithm="argon2id"))
    finally:
        c.cleanup()
        c.created = []
        for uuid in servers:
            if uuid:
                call("ref", "DELETE", f"/api/v1/servers/{uuid}", tok=TOKEN)
        time.sleep(3)
        for uuid in servers:
            rid = sql(f"SELECT id FROM servers WHERE uuid='{uuid}'") if uuid else ""
            if rid:
                sql(f"DELETE FROM standalone_dockers WHERE server_id={rid}")
                sql(f"DELETE FROM server_settings WHERE server_id={rid}")
                sql(f"DELETE FROM servers WHERE id={rid}")
        if key:
            call("ref", "DELETE", f"/api/v1/security/keys/{key}", tok=TOKEN)
            sql(f"DELETE FROM private_keys WHERE uuid='{key}'")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", default="hml20fsj5eles0xfiqidq8j5")
    ap.add_argument("--project", default="j8xm2emlllhfnyebjzgyzp7z")
    ap.add_argument("--part", default="all", choices=["all", "create", "image", "update", "caddy"])
    a = ap.parse_args()
    c = Cases({"server_uuid": a.server, "project_uuid": a.project, "environment_name": "production"})
    try:
        if a.part in ("all", "create"):
            run_git_create(c)
        if a.part in ("all", "image"):
            run_image_create(c)
        if a.part in ("all", "update"):
            run_update(c)
        if a.part in ("all", "caddy"):
            run_caddy(c)
    finally:
        c.cleanup()
    passed = sum(1 for r in results if r)
    print(f"\n{passed}/{len(results)} checks identical")
    sys.exit(0 if passed == len(results) else 1)


if __name__ == "__main__":
    main()
