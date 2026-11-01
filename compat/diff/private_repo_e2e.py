#!/usr/bin/env python3
"""Private repositories end to end: source, repository list, branch list, application, deployment.

  gitea   a real local Gitea container (gitea/gitea): a real OAuth app, token and refresh, real clone
  gitlab  compat/diff/fake_git_provider.py
  github  compat/diff/fake_git_provider.py: installation token, repository and branch lists, private deploy

coolify-rs creates the application through its page endpoints. The reference deploys an application with
the same attributes through its own API, and the deployment commands, log lines and Compose files are
compared. A real GitHub App or GitLab still stays with the user: the fake only answers documented shapes.

    python3 compat/diff/fake_git_provider.py --port 18990 &
    GITEA_HOST=<lan address> FAKE_BASE=http://<lan address>:18990 COOLIFY_RS_BASE=http://127.0.0.1:18091 \
        python3 compat/diff/private_repo_e2e.py --case gitea,gitlab,github
"""
import argparse
import difflib
import html
import http.cookiejar
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
os.environ.setdefault("FAKE_BASE", "http://127.0.0.1:18990")
from databases_diff import call, record, results, sql  # noqa: E402
from git_source_deploy_diff import ENV, FAKE, PROXY, SERVER, entries, php, vm, wait  # noqa: E402
from integrations_diff import PROJECT, login  # noqa: E402

GITEA_HOST = os.environ.get("GITEA_HOST", "127.0.0.1")
GITEA_PORT = 18992
GITEA = f"http://{GITEA_HOST}:{GITEA_PORT}"
GITEA_LOCAL = f"http://127.0.0.1:{GITEA_PORT}"
CONTAINER = "coolify-rs-diff-gitea"
GITEA_USER = "e2eowner"
# A throwaway user of a throwaway container.
GITEA_PASSWORD = "e2e-Local-Only-1!"
PREFIX = "e2erepo"
tracked = {"source": None, "key": None, "apps": []}


def gitea_api(method, path, token=None, body=None, basic=False):
    req = urllib.request.Request(GITEA_LOCAL + "/api/v1" + path, method=method, data=json.dumps(body).encode() if body is not None else None)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "token " + token)
    with urllib.request.urlopen(req, timeout=30) as r:
        raw = r.read()
    return json.loads(raw) if raw else None


def start_gitea():
    subprocess.run(["docker", "rm", "-f", CONTAINER], capture_output=True)
    subprocess.run(["docker", "run", "-d", "--name", CONTAINER, "-p", f"{GITEA_PORT}:3000",
                    "-e", "GITEA__security__INSTALL_LOCK=true", "-e", "GITEA__database__DB_TYPE=sqlite3",
                    "-e", f"GITEA__server__ROOT_URL={GITEA}/", "-e", "GITEA__service__DISABLE_REGISTRATION=true",
                    "-e", "GITEA__webhook__ALLOWED_HOST_LIST=private", "gitea/gitea:1.22"], check=True, capture_output=True)
    for _ in range(60):
        try:
            gitea_api("GET", "/version")
            break
        except Exception:
            time.sleep(1)
    subprocess.run(["docker", "exec", "-u", "git", CONTAINER, "gitea", "admin", "user", "create", "--admin", "--username", GITEA_USER,
                    "--password", GITEA_PASSWORD, "--email", "e2e@example.test", "--must-change-password=false"], check=True, capture_output=True)
    token = subprocess.run(["docker", "exec", "-u", "git", CONTAINER, "gitea", "admin", "user", "generate-access-token", "--username", GITEA_USER,
                            "--scopes", "all", "--raw"], check=True, capture_output=True, text=True).stdout.strip()
    gitea_api("POST", "/user/repos", token, {"name": "secret-app", "private": True, "default_branch": "main", "auto_init": False})
    with tempfile.TemporaryDirectory() as d:
        open(os.path.join(d, "Dockerfile"), "w").write("FROM nginx:alpine\nEXPOSE 80\n")
        for args in (["init", "-q", "-b", "main"], ["add", "Dockerfile"], ["-c", "user.name=e2e", "-c", "user.email=e@example.test", "commit", "-qm", "first"],
                     ["branch", "feature"],
                     ["push", "-q", f"http://{GITEA_USER}:{token}@127.0.0.1:{GITEA_PORT}/{GITEA_USER}/secret-app.git", "main", "feature"]):
            subprocess.run(["git", "-C", d, *args], check=True, capture_output=True)
    # Gitea records pushed branches in a background queue.
    for _ in range(30):
        if len(gitea_api("GET", f"/repos/{GITEA_USER}/secret-app/branches", token) or []) == 2:
            break
        time.sleep(1)
    app = gitea_api("POST", "/user/applications/oauth2", token, {"name": "coolify-e2e", "redirect_uris": ["http://127.0.0.1:18091/webhooks/source/gitea/callback"], "confidential_client": True})
    return app["client_id"], app["client_secret"], oauth_code_flow(app["client_id"], app["client_secret"])


def oauth_code_flow(client_id, client_secret):
    """A real authorization code flow: login form, consent page, code, tokens."""
    opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))

    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *a, **k):
            return None

    def csrf(page):
        m = re.search(r'name="_csrf" value="([^"]+)"', page) or re.search(r"csrfToken: '([^']+)'", page)
        return m[1]

    page = opener.open(GITEA_LOCAL + "/user/login").read().decode()
    data = urllib.parse.urlencode({"_csrf": csrf(page), "user_name": GITEA_USER, "password": GITEA_PASSWORD}).encode()
    opener.open(GITEA_LOCAL + "/user/login", data)
    redirect = "http://127.0.0.1:18091/webhooks/source/gitea/callback"
    url = GITEA_LOCAL + "/login/oauth/authorize?" + urllib.parse.urlencode({"client_id": client_id, "redirect_uri": redirect, "response_type": "code", "state": "s1"})
    page = opener.open(url).read().decode()
    form = {"_csrf": csrf(page), "client_id": client_id, "state": "s1", "redirect_uri": redirect, "granted": "true"}
    no_redirect = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(opener.handlers[-1].cookiejar if hasattr(opener.handlers[-1], "cookiejar") else None), NoRedirect)
    for h in opener.handlers:
        if isinstance(h, urllib.request.HTTPCookieProcessor):
            no_redirect = urllib.request.build_opener(h, NoRedirect)
    try:
        no_redirect.open(GITEA_LOCAL + "/login/oauth/grant", urllib.parse.urlencode(form).encode())
        raise SystemExit("no redirect from the consent page")
    except urllib.error.HTTPError as e:
        location = e.headers["Location"]
    code = urllib.parse.parse_qs(urllib.parse.urlsplit(html.unescape(location)).query)["code"][0]
    req = urllib.request.Request(GITEA_LOCAL + "/login/oauth/access_token", method="POST", data=json.dumps({
        "client_id": client_id, "client_secret": client_secret, "code": code, "grant_type": "authorization_code", "redirect_uri": redirect,
    }).encode())
    req.add_header("Content-Type", "application/json")
    req.add_header("Accept", "application/json")
    with urllib.request.urlopen(req) as r:
        return json.loads(r.read())


def make_source(case, gitea=None):
    # An expired access token: both sides must refresh it against the provider before they use it.
    if case == "gitea":
        client_id, secret, tokens = gitea
        src = (f"App\\Models\\GiteaApp::create(['name' => '{PREFIX}-gt', 'team_id' => $team, 'api_url' => '{GITEA}/api/v1', 'html_url' => '{GITEA}', "
               f"'client_id' => '{client_id}', 'client_secret' => '{secret}', 'access_token' => '{tokens['access_token']}', 'refresh_token' => '{tokens['refresh_token']}', 'expires_at' => time() - 100, 'webhook_secret' => 'w'])")
    elif case == "gitlab":
        src = (f"App\\Models\\GitlabApp::create(['name' => '{PREFIX}-gl', 'team_id' => $team, 'api_url' => '{FAKE}/gitlab/api/v4', 'html_url' => '{FAKE}/gl', 'client_id' => 'c', 'client_secret' => 's', "
               "'access_token' => 'fixed+token', 'refresh_token' => 'r', 'expires_at' => time() - 100, 'webhook_token' => 'w', 'redirect_uri' => 'http://example.test/cb'])")
    else:
        src = (f"App\\Models\\GithubApp::create(['name' => '{PREFIX}-gh', 'team_id' => $team, 'api_url' => '{FAKE}/github', 'html_url' => 'http://{PROXY}', 'app_id' => 4242, 'installation_id' => 99, "
               "'client_id' => 'c', 'client_secret' => 's', 'webhook_secret' => 'w', 'private_key_id' => $key->id])")
    out = php(f"""
$server = App\\Models\\Server::where('uuid', '{SERVER}')->first(); $team = $server->team_id;
$key = App\\Models\\PrivateKey::create(['name' => '{PREFIX}-key', 'team_id' => $team, 'private_key' => App\\Models\\PrivateKey::generateNewKeyPair('rsa')['private_key']]);
$src = {src};
echo 'ids='.$src->id.','.$key->id.','.$server->standaloneDockers()->first()->uuid.PHP_EOL;""")
    a, b, dest = [line for line in out.splitlines() if line.startswith("ids=")][0][4:].split(",")
    tracked["source"] = (case, int(a))
    tracked["key"] = int(b)
    return int(a), dest


def run(case, ui, gitea=None):
    source_id, destination = make_source(case, gitea)
    code, sources = ui("GET", f"/ui/private-repository/{case}/sources")
    record(f"[{case}] the source is offered", code == 200 and any(s["id"] == source_id for s in sources["sources"]), sources)
    code, repos = ui("POST", f"/ui/private-repository/{case}/repositories", {"source_id": source_id, "page": 1})
    names = [r.get("full_name") or r.get("path_with_namespace") for r in repos.get("repositories", [])] if code == 200 else []
    print(f"      [{case}] repositories: {names}")
    want = {"gitea": f"{GITEA_USER}/secret-app", "gitlab": "o/r", "github": "o/r"}[case]
    record(f"[{case}] repository list", want in names, repos)
    repo = next(r for r in repos["repositories"] if (r.get("full_name") or r.get("path_with_namespace")) == want)
    code, branches = ui("POST", f"/ui/private-repository/{case}/branches", {"source_id": source_id, "repository": want, "project_id": repo.get("id")})
    branch_names = sorted(b["name"] for b in branches.get("branches", [])) if code == 200 else []
    record(f"[{case}] branch list", branch_names == ["feature", "main"], branches)
    code, created = ui("POST", f"/ui/private-repository/{case}", {
        "source_id": source_id, "repository": want, "git_branch": "main", "repository_project_id": repo.get("id"), "build_pack": "dockerfile", "port": "80",
        "project_uuid": PROJECT, "environment_uuid": ENV, "destination_uuid": destination, "base_directory": "/",
    })
    record(f"[{case}] application created by coolify-rs", code in (200, 201) and "uuid" in created, created)
    if code not in (200, 201):
        return
    us = created["uuid"]
    tracked["apps"].append(us)
    ur = php(f"""
$o = App\\Models\\Application::where('uuid', '{us}')->first();
$a = new App\\Models\\Application;
$a->forceFill($o->only(['name', 'git_repository', 'git_branch', 'repository_project_id', 'source_id', 'source_type', 'build_pack', 'ports_exposes', 'environment_id', 'destination_id', 'destination_type', 'base_directory', 'dockerfile_location', 'static_image']));
$a->save();
$a->fqdn = str_replace($o->uuid, $a->uuid, $o->fqdn); $a->save();
echo 'uuid='.$a->uuid.PHP_EOL;""").split("uuid=")[1].split()[0]
    tracked["apps"].append(ur)
    # One side after the other: a refresh rotates the refresh token, and each side starts with an expired token.
    table = {"gitea": "gitea_apps", "gitlab": "gitlab_apps", "github": None}[case]
    done = {}
    for side, u in (("ref", ur), ("rs", us)):
        if table:
            sql(f"UPDATE {table} SET expires_at = extract(epoch from now())::int - 100 WHERE id = {source_id}")
        c, r = call(side, "POST", f"/api/v1/deploy?uuid={u}", {}, tok="acme_owner_root")
        record(f"[{case}] deploy accepted by {side}", c == 200, f"{c} {r}")
        done[side] = wait(u)
    qr, qs = done["ref"], done["rs"]
    record(f"[{case}] both finished", qr and qs and qr["status"] == qs["status"] == "finished", f"{qr and qr['status']} / {qs and qs['status']}")
    if not (qr and qs):
        return
    (vr, cr), (vs, cs) = entries(qr, ur, "ref"), entries(qs, us, "rs")
    pairs = [(ur, "U"), (us, "U")]

    def clean(lines):
        out = []
        for line in lines:
            for a, b in pairs:
                line = line.replace(a, b)
            out.append(re.sub(r"(oauth2|x-access-token):[^@'\" ]+@", r"\1:TOKEN@", line))
        return out

    tv, ts = clean(f"{t} {o}" for t, o in vr), clean(f"{t} {o}" for t, o in vs)
    record(f"[{case}] visible log entries identical", tv == ts, "\n".join(difflib.unified_diff(tv, ts, lineterm="", n=0)))
    record(f"[{case}] logged commands identical", clean(cr) == clean(cs), "\n".join(difflib.unified_diff(clean(cr), clean(cs), lineterm="", n=0)))
    hosts = [sql(f"SELECT fqdn FROM applications WHERE uuid = '{u}'").split("://", 1)[1] for u in (ur, us)]
    codes = [vm(f"curl -s -o /dev/null -w '%{{http_code}}' -H 'Host: {h}' http://127.0.0.1/") for h in hosts]
    record(f"[{case}] both answer through the proxy", codes[0] == codes[1] == "200", codes)
    if case == "gitea":
        record(f"[{case}] the refresh stored a new expiry", int(sql(f"SELECT expires_at - extract(epoch from now())::int FROM gitea_apps WHERE id = {source_id}") or -1) > 60, "")


def teardown():
    for u in tracked["apps"]:
        call("ref", "DELETE", f"/api/v1/applications/{u}?delete_volumes=true", tok="acme_owner_root")
    time.sleep(6 if tracked["apps"] else 0)
    tracked["apps"].clear()
    if tracked["source"]:
        case, sid = tracked["source"]
        table = {"gitea": "gitea_apps", "gitlab": "gitlab_apps", "github": "github_apps"}[case]
        sql(f"DELETE FROM {table} WHERE id = {sid}")
        tracked["source"] = None
    if tracked["key"]:
        sql(f"DELETE FROM private_keys WHERE id = {tracked['key']}")
        tracked["key"] = None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--case", default="gitea,gitlab,github")
    cases = ap.parse_args().case.split(",")
    ui = login()
    try:
        for case in cases:
            gitea = None
            if case == "gitea":
                gitea = start_gitea()
            if case == "github":
                target = FAKE.split("://", 1)[1]
                vm(f"docker rm -f {PROXY} >/dev/null 2>&1; docker run -d --name {PROXY} --network coolify alpine/socat tcp-listen:80,fork,reuseaddr tcp:{target}")
            try:
                run(case, ui, gitea)
            finally:
                teardown()
                if case == "gitea":
                    subprocess.run(["docker", "rm", "-f", CONTAINER], capture_output=True)
                if case == "github":
                    vm(f"docker rm -f {PROXY} >/dev/null 2>&1")
    finally:
        teardown()
    failed = results.count(False)
    print(f"\n{len(results) - failed}/{len(results)} checks identical")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
