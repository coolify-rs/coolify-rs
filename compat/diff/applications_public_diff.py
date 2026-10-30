#!/usr/bin/env python3
"""Differential check of POST /applications/public: upstream reference vs coolify-rs.

Same setup as applications_diff.py (reference on :18000, coolify-rs on :18080 against the
reference database). Usage:

    applications_public_diff.py [--server <uuid>] [--project <uuid>]

Creates applications from public git repositories for every build pack (instant_deploy
false) through both implementations and compares the responses, the rows each side
writes (application, settings, environment variables, generated labels, config hash),
the repository normalisation (GitHub URL forms, SSH URLs, other hosts), the generated
names, and the validation failures. Nothing is deployed. Everything it creates is
deleted at the end. Exit status 1 when anything differs.
"""
import argparse
import difflib
import json
import os
import re
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from applications_fields_diff import Cases, run_caddy, run_git_create, run_update  # noqa: E402
from databases_diff import call, record, results, sql  # noqa: E402

ENDPOINT = "/api/v1/applications/public"
REPO = "https://github.com/coollabsio/coolify-examples"
TOKEN = "acme_owner_root"
SKIP = {"id", "uuid", "created_at", "updated_at", "last_online_at", "manual_webhook_secret_github",
        "manual_webhook_secret_gitlab", "manual_webhook_secret_bitbucket", "manual_webhook_secret_gitea"}


def app_row(u):
    return json.loads(sql(f"SELECT row_to_json(x)::text FROM applications x WHERE uuid='{u}'") or "null")


def settings_row(app_id):
    r = json.loads(sql(f"SELECT row_to_json(s)::text FROM application_settings s WHERE application_id={app_id}") or "null")
    for k in ("id", "application_id", "created_at", "updated_at"):
        r and r.pop(k)
    return r


def env_rows(app_id):
    rows = [json.loads(x) for x in sql(
        f"SELECT row_to_json(v)::text FROM environment_variables v WHERE resourceable_type='App\\Models\\Application' "
        f"AND resourceable_id={app_id} ORDER BY id").splitlines() if x]
    return rows


def php(code):
    """Runs PHP inside the bootstrapped reference application."""
    return subprocess.run(["docker", "exec", "coolify-rs-reference-coolify-1", "php", "-r",
                           "require '/var/www/html/vendor/autoload.php'; $app = require '/var/www/html/bootstrap/app.php'; "
                           "$app->make(Illuminate\\Contracts\\Console\\Kernel::class)->bootstrap(); " + code],
                          capture_output=True, text=True).stdout.strip()


def decrypt(value):
    return php(f"echo decrypt('{value}');")


def upstream_hash(uuid):
    """What upstream's isConfigurationChanged(true) computes for a row (labels hold the uuid,
    so hashes of two applications never match; each side's row is checked by upstream)."""
    return php(f"echo App\\Models\\Application::where('uuid', '{uuid}')->first()->deploymentConfigurationHash();")


def new_key():
    """A throwaway ed25519 private key (OpenSSH format)."""
    import tempfile
    d = tempfile.mkdtemp()
    subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "diff", "-f", d + "/k"], check=True)
    return open(d + "/k").read()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", default="hml20fsj5eles0xfiqidq8j5")
    ap.add_argument("--project", default="j8xm2emlllhfnyebjzgyzp7z")
    a = ap.parse_args()
    base = {"server_uuid": a.server, "project_uuid": a.project, "environment_name": "production"}
    created = []
    keys = []

    def post(side, body, path=ENDPOINT, tok=TOKEN):
        c, r = call(side, "POST", path, body, tok=tok)
        try:
            j = json.loads(r)
        except ValueError:
            j = None
        if c == 201 and isinstance(j, dict) and j.get("uuid"):
            created.append(j["uuid"])
        return c, r, j

    def same_error(name, body, path=ENDPOINT, tok=TOKEN):
        ra, rb = post("ref", body, path, tok), post("rs", body, path, tok)
        record(name, (ra[0], ra[1]) == (rb[0], rb[1]), f"ref {ra[0]} {ra[1][:600]}\n  rs  {rb[0]} {rb[1][:600]}")

    def pair(name, body, ref_extra=None, rs_extra=None, compare_name=True, compare_fqdn=True, compare_hash=True):
        return pair_at(ENDPOINT, name, body, ref_extra, rs_extra, compare_name, compare_fqdn, compare_hash)

    def pair_at(path, name, body, ref_extra=None, rs_extra=None, compare_name=True, compare_fqdn=True, compare_hash=True):
        """Creates on both sides and compares response shape and rows. Returns the two rows."""
        ra = post("ref", dict(body, **(ref_extra or {})), path)
        rb = post("rs", dict(body, **(rs_extra or {})), path)
        ok = ra[0] == rb[0] == 201 and list(ra[2] or {}) == list(rb[2] or {})
        record(f"{name}: create status and shape", ok, f"ref {ra[0]} {ra[1][:400]}\n  rs  {rb[0]} {rb[1][:400]}")
        if not ok:
            return None, None
        ua, ub = ra[2]["uuid"], rb[2]["uuid"]
        if compare_fqdn:
            record(f"{name}: response domains", ra[2]["domains"] == rb[2]["domains"], f"{ra[2]} / {rb[2]}")
        else:
            record(f"{name}: response domains alike",
                   (ra[2]["domains"] or "").replace(ua, "U") == (rb[2]["domains"] or "").replace(ub, "U"), f"{ra[2]} / {rb[2]}")
        rowa, rowb = app_row(ua), app_row(ub)
        skip = set(SKIP) | {"custom_labels", "config_hash"}
        if not compare_name:
            skip.add("name")
        if not compare_fqdn:
            skip |= {"fqdn", "domain_port_overrides"}
        diff = {k: (rowa[k], rowb.get(k)) for k in rowa if k not in skip and rowa[k] != rowb.get(k)}
        record(f"{name}: application rows alike", not diff, diff)
        if not compare_fqdn:
            record(f"{name}: fqdn alike", (rowa["fqdn"] or "").replace(ua, "U") == (rowb["fqdn"] or "").replace(ub, "U"),
                   f"{rowa['fqdn']} / {rowb['fqdn']}")
        la = (rowa["custom_labels"] or "").replace(ua, "U")
        lb = (rowb["custom_labels"] or "").replace(ub, "U")
        if not compare_name:
            la, lb = la.replace(rowa["name"], "NAME"), lb.replace(rowb["name"], "NAME")
        record(f"{name}: generated labels identical", la == lb,
               "\n".join(difflib.unified_diff(la.splitlines(), lb.splitlines(), lineterm="")))
        if compare_hash:
            want = upstream_hash(ub)
            record(f"{name}: config_hash is what upstream computes for the coolify-rs row",
                   rowb["config_hash"] == want and rowa["config_hash"] == upstream_hash(ua),
                   f"rs {rowb['config_hash']} upstream {want}")
        sa, sb = settings_row(rowa["id"]), settings_row(rowb["id"])
        record(f"{name}: settings rows alike", sa == sb, {k: (sa[k], sb.get(k)) for k in sa if sa[k] != sb.get(k)})
        ea, eb = env_rows(rowa["id"]), env_rows(rowb["id"])
        strip = lambda rows: [{k: v for k, v in r.items() if k not in ("id", "uuid", "resourceable_id", "value", "created_at", "updated_at")}
                              for r in rows]
        record(f"{name}: environment variable rows alike", strip(ea) == strip(eb), f"{strip(ea)}\n  {strip(eb)}")
        return rowa, rowb

    try:
        # Minimal nixpacks: generated name and domain, NIXPACKS_NODE_VERSION pair.
        ra, rb = pair("nixpacks minimal", dict(base, git_repository=REPO, git_branch="main", build_pack="nixpacks",
                                               ports_exposes="3000"),
                      compare_name=False, compare_fqdn=False, compare_hash=False)
        if ra and rb:
            shape = re.compile(r"^coolify-examples:main-[a-z0-9]{24}$")
            record("generated name shape", bool(shape.match(ra["name"]) and shape.match(rb["name"])), f"{ra['name']} / {rb['name']}")
            envs = env_rows(rb["id"])
            record("NIXPACKS_NODE_VERSION pair (rs)", [(e["key"], e["is_preview"]) for e in envs]
                   == [("NIXPACKS_NODE_VERSION", False), ("NIXPACKS_NODE_VERSION", True)], envs)
            record("NIXPACKS_NODE_VERSION values decrypt to 22 (rs)", [decrypt(e["value"]) for e in envs] == ["22", "22"])

        # Every build pack with the same name and domain on both sides: full rows incl. config hash.
        for i, (bp, extra) in enumerate([
            ("nixpacks", {"ports_exposes": "3000"}),
            ("static", {"ports_exposes": "80", "publish_directory": "/dist", "is_spa": True}),
            ("dockerfile", {"ports_exposes": "8080", "dockerfile_location": None, "base_directory": "/dockerfile"}),
            ("railpack", {"ports_exposes": "3000", "install_command": "npm ci", "build_command": None,
                          "start_command": "npm start"}),
        ]):
            pair(f"build pack {bp}", dict(base, name=f"public-diff-{bp}", git_repository=REPO, git_branch="main", build_pack=bp,
                                          domains=f"https://public-diff-{i}.example.com:8443", force_domain_override=True, **extra))
        # dockercompose: no domain, ports_exposes forced to 80 even when invalid; the compose
        # file does not exist so upstream's LoadComposeFile job leaves the row as it is.
        pair("build pack dockercompose", dict(base, name="public-diff-compose", git_repository=REPO, git_branch="main",
                                              build_pack="dockercompose", ports_exposes="bad",
                                              docker_compose_location="/missing/docker-compose.yml/"))
        pair("dockercompose raw flag", dict(base, name="public-diff-compose-raw", git_repository=REPO, git_branch="main",
                                            build_pack="dockercompose", is_raw_compose_deployment_enabled=True,
                                            docker_compose_location=None))

        # Settings, setters and quirks.
        pair("setters and settings", dict(
            base, name="public-diff-rich", git_repository=REPO + ".git", git_branch="feature/x-1", build_pack="nixpacks",
            git_commit_sha="abc123", base_directory="/nested/dir/", publish_directory=None, install_command=None,
            dockerfile_location="/docker/Dockerfile/", docker_compose_location="//compose.yml", static_image="nginx:alpine",
            is_preview_deployments_enabled=True, use_build_secrets=True, is_preserve_repository_enabled=True,
            is_git_submodules_enabled=False, is_git_lfs_enabled=False, is_git_shallow_clone_enabled=False,
            disable_build_cache=True, inject_build_args_to_dockerfile=False, include_source_commit_in_build=True,
            is_auto_deploy_enabled=False, is_force_https_enabled=False, redirect="www", health_check_path="/up",
            limits_memory="256m", autogenerate_domain=False, description="<i>d</i>"))
        pair("is_static overrides ports_exposes", dict(base, name="public-diff-static-flag", git_repository=REPO, git_branch="main",
                                                         build_pack="nixpacks", ports_exposes="3000", is_static=True,
                                                         autogenerate_domain=False))
        pair("is_static false keeps ports_exposes", dict(base, name="public-diff-static-off", git_repository=REPO, git_branch="main",
                                                           build_pack="static", ports_exposes="3000", is_static=False,
                                                           autogenerate_domain=False))

        # Repository normalisation.
        for label, url, want, source in [
            ("plain github", "https://github.com/coollabsio/coolify-examples", "coollabsio/coolify-examples", True),
            (".git kept", "https://github.com/coollabsio/coolify-examples.git", "coollabsio/coolify-examples.git", True),
            ("/tree/x trimmed", "https://github.com/coollabsio/coolify-examples/tree/v4.x", "coollabsio/coolify-examples", True),
            ("http github", "http://github.com/coollabsio/coolify-examples", "coollabsio/coolify-examples", True),
            ("owner only", "https://github.com/coollabsio", "coollabsio/", True),
            ("scp github", "git@github.com:coollabsio/coolify-examples.git", "coollabsio/coolify-examples.git", True),
            ("ssh gitlab", "ssh://git@gitlab.com:2222/o/r.git", "https://gitlab.com/o/r.git", False),
            ("scp gitlab", "git@gitlab.com:o/r.git", "https://gitlab.com/o/r.git", False),
            ("other host", "https://gitlab.com/o/r.git", "https://gitlab.com/o/r.git", False),
            ("www.github.com", "https://www.github.com/o/r", "https://www.github.com/o/r", False),
            ("percent-encoded path", "https://gitlab.com/o/r%2Dx", "https://gitlab.com/o/r%2Dx", False),
        ]:
            ra, rb = pair(f"url {label}", dict(base, git_repository=url, git_branch="Feature/ABC", build_pack="static",
                                              autogenerate_domain=False), compare_name=False, compare_hash=False)
            if ra and rb:
                got = [(r["git_repository"], r["source_type"], r["source_id"]) for r in (ra, rb)]
                exp = (want, "App\\Models\\GithubApp" if source else None, 0 if source else None)
                record(f"url {label}: stored as expected", got == [exp, exp], f"{got} / {exp}")
                record(f"url {label}: generated name alike", ra["name"][:-24] == rb["name"][:-24] and
                       re.match(r"^.+:-feature/-a-b-c-[a-z0-9]{24}$", rb["name"]) is not None, f"{ra['name']} / {rb['name']}")

        # Validation failures and early answers.
        ok = dict(base, git_repository=REPO, git_branch="main", build_pack="nixpacks")
        drop = lambda *keys: {k: v for k, v in ok.items() if k not in keys}
        for name, body in [
            ("missing git_repository", drop("git_repository")),
            ("missing git_branch", drop("git_branch")),
            ("missing build_pack", drop("build_pack")),
            ("missing all three", drop("git_repository", "git_branch", "build_pack")),
            ("invalid build pack", dict(ok, build_pack="foo")),
            ("dockerimage build pack", dict(ok, build_pack="dockerimage")),
            ("non-string repository and branch", dict(ok, git_repository=123, git_branch=True)),
            ("empty array repository", dict(ok, git_repository=[])),
            ("array repository", dict(ok, git_repository=["x"])),
            ("repo invalid characters", dict(ok, git_repository="https://github.com/o/r;ls")),
            ("repo control character", dict(ok, git_repository="https://github.com/o/r%0A")),
            ("repo invalid patterns", dict(ok, git_repository="https://github.com/o/../r")),
            ("repo invalid ssh", dict(ok, git_repository="git@github.com:o/r,x")),
            ("repo invalid ssh://", dict(ok, git_repository="ssh://github.com/o/r")),
            ("repo invalid url", dict(ok, git_repository="https://git_hub.com/o/r")),
            ("repo internal host", dict(ok, git_repository="https://localhost/o/r")),
            ("repo internal ipv6", dict(ok, git_repository="http://[::1]/o/r")),
            ("repo .local host", dict(ok, git_repository="https://box.local/o/r")),
            ("repo encoded user", dict(ok, git_repository="https://us%65r@github.com/o/r")),
            ("repo invalid path", dict(ok, git_repository="https://github.com/o/r,x")),
            ("repo invalid git://", dict(ok, git_repository="git://github.com/o/r,x")),
            ("repo no scheme", dict(ok, git_repository="github.com/o/r")),
            ("git:// passes validation then throws", dict(ok, git_repository="git://example.com/o/r.git")),
            ("branch invalid characters", dict(ok, git_branch="a b")),
            ("branch invalid patterns", dict(ok, git_branch="a//b")),
            ("branch leading slash", dict(ok, git_branch="/main")),
            ("branch unicode", dict(ok, git_branch="maïn")),
            ("branch HEAD", dict(ok, git_branch="HEAD")),
            ("branch .lock", dict(ok, git_branch="main.lock")),
            ("domains with dockercompose", dict(ok, build_pack="dockercompose", domains="https://x.example.com")),
            ("raw compose flag on nixpacks", dict(ok, is_raw_compose_deployment_enabled=True)),
            ("invalid ports_exposes", dict(ok, ports_exposes="80a")),
            ("several shared rule failures", dict(ok, is_static="x", install_command="a;b", base_directory="rel",
                                                  dockerfile_location="Dockerfile", static_image="nginx", git_commit_sha="-x",
                                                  docker_compose_custom_start_command="a|b")),
            ("null on non-nullable shared rules", dict(ok, git_commit_sha=None, is_spa=None, limits_memory=None)),
            ("invalid domain", dict(ok, domains="ftp://a.example.com")),
            ("extra field", dict(ok, fqdn="http://a.b", foo=1)),
            ("project missing", dict(ok, project_uuid="nope")),
            ("environment missing", dict(ok, environment_name="nope")),
            ("no environment", {k: v for k, v in ok.items() if k != "environment_name"}),
        ]:
            same_error(f"error: {name}", body)
        same_error("instant deploy without deploy ability", dict(ok, instant_deploy=True), tok="acme_owner_write")
        same_error("member cannot create", ok, tok="acme_member_write_legacy")
        same_error("dockerimage endpoint: null git_branch", dict(base, docker_registry_image_name="nginx", git_branch=None),
                   path="/api/v1/applications/dockerimage")

        # POST /applications/private-deploy-key: a throwaway key of the team.
        dk = "/api/v1/applications/private-deploy-key"
        c, r = call("ref", "POST", "/api/v1/security/keys", {"name": "public-diff key", "private_key": new_key()}, tok=TOKEN)
        key_uuid = json.loads(r)["uuid"]
        keys.append(key_uuid)
        key_id = sql(f"SELECT id FROM private_keys WHERE uuid='{key_uuid}'")
        kbase = dict(base, private_key_uuid=key_uuid)
        for i, (bp, extra) in enumerate([
            ("nixpacks", {"ports_exposes": "3000"}),
            ("static", {"publish_directory": "/out"}),
            ("dockerfile", {"dockerfile_location": None}),
            ("railpack", {}),
            ("dockercompose", {"docker_compose_location": "/missing.yml"}),
        ]):
            extra = dict(extra, **({} if bp == "dockercompose" else
                                   {"domains": f"https://dk-diff-{i}.example.com", "force_domain_override": True}))
            ra, rb = pair_at(dk, f"deploy key {bp}", dict(kbase, name=f"dk-diff-{bp}",
                             git_repository="git@github.com:coollabsio/coolify-examples.git", git_branch="main", build_pack=bp, **extra))
            if ra and rb:
                got = [(x["git_repository"], x["source_type"], x["source_id"], x["private_key_id"]) for x in (ra, rb)]
                exp = ("git@github.com:coollabsio/coolify-examples.git", None, None, int(key_id))
                record(f"deploy key {bp}: repository kept, key attached", got == [exp, exp], got)
        pair_at(dk, "deploy key https url", dict(kbase, git_repository=REPO + ".git", git_branch="main", build_pack="static",
                                                 autogenerate_domain=False), compare_name=False, compare_hash=False)
        pair_at(dk, "deploy key git:// url stored as sent", dict(kbase, name="dk-git-proto", git_repository="git://example.com/o/r.git",
                                                                 git_branch="main", build_pack="static", autogenerate_domain=False))
        okd = dict(kbase, git_repository="git@github.com:o/r.git", git_branch="main", build_pack="nixpacks")
        for name, body in [
            ("missing private_key_uuid", {k: v for k, v in okd.items() if k != "private_key_uuid"}),
            ("unknown private key", dict(okd, private_key_uuid="nope")),
            ("missing repository and branch", {k: v for k, v in okd.items() if k not in ("git_repository", "git_branch")}),
            ("invalid repository", dict(okd, git_repository="git@github.com:o/r,x")),
            ("array repository", dict(okd, git_repository=["x"])),
            ("compose ports_exposes validated", dict(okd, build_pack="dockercompose", ports_exposes="bad")),
            ("domains with dockercompose", dict(okd, build_pack="dockercompose", domains="https://x.example.com")),
        ]:
            same_error(f"deploy key error: {name}", body, path=dk)

        # POST /applications/private-github-app: no GitHub App exists in the reference, so only
        # the paths before the GitHub API call are compared.
        gh = "/api/v1/applications/private-github-app"
        okg = dict(base, github_app_uuid="nope", git_repository="coollabsio/coolify-examples", git_branch="main",
                   build_pack="nixpacks")
        for name, body in [
            ("unknown github app", okg),
            ("system app of another team is not found", dict(okg, github_app_uuid="github-public-nope")),
            ("missing github_app_uuid", {k: v for k, v in okg.items() if k != "github_app_uuid"}),
            ("missing everything git", {k: v for k, v in okg.items() if k not in ("github_app_uuid", "git_repository", "git_branch", "build_pack")}),
            ("repository is not URL-validated", dict(okg, git_repository="not a url;")),
            ("array repository does not throw", dict(okg, git_repository=["x"])),
            ("array branch throws", dict(okg, git_branch=["x"])),
            ("invalid branch", dict(okg, git_branch="a b")),
            ("invalid build pack", dict(okg, build_pack="dockerimage")),
            ("compose ports_exposes validated", dict(okg, build_pack="dockercompose", ports_exposes="bad")),
            ("domains with dockercompose", dict(okg, build_pack="dockercompose", domains="https://x.example.com")),
            ("raw compose flag on nixpacks", dict(okg, is_raw_compose_deployment_enabled=True)),
            ("non-string github_app_uuid", dict(okg, github_app_uuid=5)),
        ]:
            same_error(f"github app error: {name}", body, path=gh)

        # Every other create field, compose domains, PATCH /applications/{uuid}.
        fields = Cases(base)
        try:
            run_git_create(fields)
            run_update(fields)
            run_caddy(fields)
        finally:
            fields.cleanup()
    finally:
        for u in created:
            call("ref", "DELETE", f"/api/v1/applications/{u}", tok=TOKEN)
        time.sleep(10)
        for u in created:
            rid = sql(f"SELECT id FROM applications WHERE uuid='{u}'")
            if rid:
                sql(f"DELETE FROM environment_variables WHERE resourceable_type='App\\Models\\Application' AND resourceable_id={rid}")
                sql(f"DELETE FROM application_settings WHERE application_id={rid}")
                sql(f"DELETE FROM applications WHERE id={rid}")
        for k in keys:
            call("ref", "DELETE", f"/api/v1/security/keys/{k}", tok=TOKEN)
            sql(f"DELETE FROM private_keys WHERE uuid='{k}'")
    passed = sum(1 for r in results if r)
    print(f"\n{passed}/{len(results)} checks identical")
    sys.exit(0 if passed == len(results) else 1)


if __name__ == "__main__":
    main()
