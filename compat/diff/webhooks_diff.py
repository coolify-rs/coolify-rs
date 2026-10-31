#!/usr/bin/env python3
"""Differential check of `/webhooks/source/*`: real signed payloads sent to the reference and to
coolify-rs. Both sides read the same rows, created through upstream's models.

Provider API calls go to compat/diff/fake_git_provider.py on FAKE_BASE. The applications sit on a
fixture server with `concurrent_builds = 0`, so deployments stay queued and nothing is built; the
queue and preview rows each side writes are compared and removed after every case.

    COOLIFY_RS_BASE=http://127.0.0.1:18085 webhooks_diff.py

Everything it creates is deleted. Exit status 1 when anything differs.
"""
import hashlib
import hmac
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from databases_diff import BASES, record, results, sql  # noqa: E402

FAKE = os.environ["FAKE_BASE"]
PROJECT = "j8xm2emlllhfnyebjzgyzp7z"
ENV = "uj3st9yvtvzxh7rul3c3opcz"
PREFIX = "whdiff"
GH_SECRET, GL_TOKEN, GT_SECRET = "gh-webhook-secret", "gl-webhook-token", "gt-webhook-secret"
MANUAL = {k: f"manual-{k}-secret" for k in ("github", "gitlab", "gitea", "bitbucket")}
SHA_SIDE = {"ref": "a" * 39, "rs": "b" * 39}
UUID = re.compile(r'"deployment_uuid":"[a-z0-9]+"')


def php(code):
    r = subprocess.run(["docker", "exec", "coolify-rs-reference-coolify-1", "php", "/var/www/html/artisan", "tinker", "--execute", code],
                       capture_output=True, text=True, check=True)
    return r.stdout.strip()


def setup():
    out = php(f"""
$team = App\\Models\\Project::where('uuid', '{PROJECT}')->first()->team_id;
$key = App\\Models\\PrivateKey::create(['name' => '{PREFIX}-key', 'team_id' => $team, 'private_key' => App\\Models\\PrivateKey::generateNewKeyPair('rsa')['private_key']]);
$server = App\\Models\\Server::create(['name' => '{PREFIX}-server', 'ip' => '203.0.113.77', 'team_id' => $team, 'private_key_id' => $key->id, 'proxy' => ['type' => 'none']]);
$server->settings()->update(['is_reachable' => true, 'is_usable' => true, 'concurrent_builds' => 0, 'force_disabled' => false]);
$dest = $server->standaloneDockers()->first() ?? App\\Models\\StandaloneDocker::create(['name' => '{PREFIX}-dest', 'network' => '{PREFIX}-net', 'server_id' => $server->id]);
$gh = App\\Models\\GithubApp::create(['name' => '{PREFIX}-gh', 'team_id' => $team, 'api_url' => '{FAKE}/github', 'html_url' => '{FAKE}/gh', 'app_id' => 987654, 'installation_id' => 99, 'client_id' => 'c', 'client_secret' => 's', 'webhook_secret' => '{GH_SECRET}', 'private_key_id' => $key->id]);
$gl = App\\Models\\GitlabApp::create(['name' => '{PREFIX}-gl', 'team_id' => $team, 'api_url' => '{FAKE}/gitlab/api/v4', 'html_url' => '{FAKE}/gl', 'webhook_token' => '{GL_TOKEN}']);
$gt = App\\Models\\GiteaApp::create(['name' => '{PREFIX}-gt', 'team_id' => $team, 'api_url' => '{FAKE}/gitea/api/v1', 'html_url' => '{FAKE}/gt', 'webhook_secret' => '{GT_SECRET}']);
$env = App\\Models\\Environment::where('uuid', '{ENV}')->first();
$mk = function ($name, $attrs) use ($env, $dest) {{
    $a = new App\\Models\\Application;
    $a->forceFill($attrs + ['name' => '{PREFIX}-'.$name, 'git_branch' => 'main', 'build_pack' => 'dockerfile', 'ports_exposes' => '80', 'environment_id' => $env->id, 'destination_id' => $dest->id, 'destination_type' => $dest->getMorphClass(), 'fqdn' => 'http://'.$name.'.example.test', 'preview_url_template' => '{{{{pr_id}}}}.{{{{domain}}}}']);
    $a->save();
    return $a;
}};
$apps = [
  'gh' => $mk('gh', ['git_repository' => 'o/r', 'repository_project_id' => 1234, 'source_id' => $gh->id, 'source_type' => $gh->getMorphClass()]),
  'gl' => $mk('gl', ['git_repository' => 'o/r', 'repository_project_id' => 2345, 'source_id' => $gl->id, 'source_type' => $gl->getMorphClass()]),
  'gt' => $mk('gt', ['git_repository' => 'o/r', 'repository_project_id' => 3456, 'source_id' => $gt->id, 'source_type' => $gt->getMorphClass()]),
  'mn' => $mk('mn', ['git_repository' => 'https://git.example.test/acme/manual.git', 'manual_webhook_secret_github' => '{MANUAL["github"]}', 'manual_webhook_secret_gitlab' => '{MANUAL["gitlab"]}', 'manual_webhook_secret_gitea' => '{MANUAL["gitea"]}', 'manual_webhook_secret_bitbucket' => '{MANUAL["bitbucket"]}']),
];
foreach ($apps as $n => $a) {{ echo $n.'='.$a->id.PHP_EOL; }}
""")
    return {k: int(v) for k, v in (line.split("=") for line in out.splitlines() if "=" in line)}


def teardown():
    php(f"""
$apps = App\\Models\\Application::withTrashed()->where('name', 'like', '{PREFIX}-%')->get();
foreach ($apps as $a) {{
    DB::table('application_deployment_queues')->where('application_id', (string) $a->id)->delete();
    DB::table('application_previews')->where('application_id', $a->id)->delete();
    DB::table('application_settings')->where('application_id', $a->id)->delete();
    DB::table('applications')->where('id', $a->id)->delete();
}}
foreach (['github_apps', 'gitlab_apps', 'gitea_apps'] as $t) {{ DB::table($t)->where('name', 'like', '{PREFIX}-%')->delete(); }}
foreach (App\\Models\\Server::where('name', '{PREFIX}-server')->get() as $s) {{
    DB::table('standalone_dockers')->where('server_id', $s->id)->delete();
    DB::table('server_settings')->where('server_id', $s->id)->delete();
    DB::table('servers')->where('id', $s->id)->delete();
}}
DB::table('private_keys')->where('name', '{PREFIX}-key')->delete();
""")


def post(side, path, body, headers):
    raw = body if isinstance(body, bytes) else json.dumps(body).encode()
    req = urllib.request.Request(BASES[side] + path, data=raw, method="POST")
    for k, v in headers.items():
        req.add_header(k, v)
    try:
        r = urllib.request.urlopen(req)
        return r.status, r.headers.get("Retry-After"), r.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.headers.get("Retry-After"), e.read().decode()


def sign(secret, raw):
    return hmac.new(secret.encode(), raw, hashlib.sha256).hexdigest()


def norm(text):
    text = UUID.sub('"deployment_uuid":"<v>"', text.replace(": ", ":").replace(", ", ","))
    for sha in SHA_SIDE.values():
        text = text.replace(sha, "<sha>")
    try:
        return json.dumps(json.loads(text))
    except ValueError:
        return text


def queue_rows(app):
    rows = sql(f"SELECT coalesce(json_agg(t ORDER BY id), '[]') FROM (SELECT id, pull_request_id, force_rebuild, status, is_webhook, git_type, commit, restart_only, rollback, application_name, server_name, only_this_server, is_api FROM application_deployment_queues WHERE application_id = '{app}') t")
    out = json.loads(rows or "[]")
    for r in out:
        r.pop("id")
        for sha in SHA_SIDE.values():
            r["commit"] = (r.get("commit") or "").replace(sha, "<sha>")
    return out


def preview_rows(app):
    rows = sql(f"SELECT coalesce(json_agg(t ORDER BY id), '[]') FROM (SELECT id, pull_request_id, pull_request_html_url, git_type, fqdn, status, docker_compose_domains, deleted_at IS NOT NULL AS trashed FROM application_previews WHERE application_id = {app}) t")
    out = json.loads(rows or "[]")
    for r in out:
        r.pop("id")
    return out


def clear(app):
    sql(f"DELETE FROM application_deployment_queues WHERE application_id = '{app}'")
    sql(f"DELETE FROM application_previews WHERE application_id = {app}")


def settle(app, want_pr=False, wait=60):
    """The reference handles PR webhooks on a queue: wait until its rows appear."""
    if not want_pr:
        return
    end = time.time() + wait
    while time.time() < end:
        if queue_rows(app) or preview_rows(app):
            return
        time.sleep(0.5)


def case(name, path, app, build, want_pr=False, setup_sql=None, keep=False, pause=0.0, compare_rows=True):
    """`build(sha)` gives (body, headers). Compares answers, then the rows of each side."""
    answers, rows = [], []
    for side in ("ref", "rs"):
        if setup_sql:
            sql(setup_sql)
        body, headers = build(SHA_SIDE[side] + ("1" if side == "ref" else "2"))
        answers.append(post(side, path, body, headers))
        settle(app, want_pr)
        if pause:
            time.sleep(pause)
        rows.append((queue_rows(app), preview_rows(app)))
        if not keep:
            clear(app)
    (sa, ra, ta), (sb, rb, tb) = answers
    # A failure page is rendered HTML upstream; only its status compares.
    if sa >= 500 and sb >= 500:
        ta = tb = ""
    ok = (sa, ra, norm(ta).replace(SHA_SIDE["ref"] + "1", "<sha>")) == (sb, rb, norm(tb).replace(SHA_SIDE["rs"] + "2", "<sha>"))
    record(name, ok, f"\n   ref {sa} {ta[:900]}\n   rs  {sb} {tb[:900]}")
    a, b = json.dumps(rows[0]), json.dumps(rows[1])
    norm_rows = lambda s: re.sub(r"<sha>[12]", "<sha>", s)
    a, b = norm_rows(a), norm_rows(b)
    if compare_rows:
        record(f"{name}: rows", a == b, f"\n   ref {a[:900]}\n   rs  {b[:900]}")


def github_cases(apps):
    app = apps["gh"]
    url = "/webhooks/source/github/events"

    def sent(event, payload, secret=GH_SECRET, target="987654", signature=None):
        def build(sha):
            p = json.loads(json.dumps(payload).replace("SHA", sha))
            raw = json.dumps(p).encode()
            return raw, {"Content-Type": "application/json", "X-GitHub-Event": event, "X-GitHub-Hook-Installation-Target-Id": target,
                         "X-Hub-Signature-256": signature if signature is not None else "sha256=" + sign(secret, raw)}
        return build

    push = {"ref": "refs/heads/main", "after": "SHA", "before": "c" * 40, "repository": {"id": 1234, "full_name": "o/r"}, "installation": {"id": 99},
            "commits": [{"message": "Work", "added": ["src/a.js"], "removed": [], "modified": []}]}
    pr = {"action": "opened", "number": 7, "repository": {"id": 1234, "full_name": "o/r"}, "installation": {"id": 99},
          "pull_request": {"html_url": "https://example.test/o/r/pull/7", "title": "A change",
                           "head": {"ref": "feature", "sha": "SHA", "repo": {"id": 1234, "full_name": "o/r"}},
                           "base": {"ref": "main", "repo": {"id": 1234, "full_name": "o/r"}}}}
    case("github ping", url, app, sent("ping", {}))
    case("github unknown app", url, app, sent("push", push, target="11"))
    case("github no target", url, app, sent("push", push, target=""))
    case("github wrong signature", url, app, sent("push", push, secret="nope"))
    case("github missing signature", url, app, sent("push", push, signature=""))
    case("github other event", url, app, sent("issues", {"action": "x"}))
    case("github installation", url, app, sent("installation", {"action": "created"}))
    case("github push", url, app, sent("push", push))
    case("github push branch without apps", url, app, sent("push", {**push, "ref": "refs/heads/other"}))
    case("github push no branch", url, app, sent("push", {**push, "ref": None}))
    case("github push bad id", url, app, sent("push", {**push, "repository": {"id": "abc", "full_name": "o/r"}}))
    case("github push bad sha", url, app, sent("push", {**push, "after": "zzz"}))
    case("github push branch deleted", url, app, sent("push", {**push, "deleted": True}))
    case("github push zero after", url, app, sent("push", {**push, "after": "0" * 40}))
    case("github push skip ci", url, app, sent("push", {**push, "commits": [{"message": "x [skip ci]", "added": ["a"]}]}))
    case("github push watch paths, no match in compare", url, app, sent("push", push),
         setup_sql=f"UPDATE applications SET watch_paths = 'nothing/**' WHERE id = {app}")
    case("github push watch paths match", url, app, sent("push", push),
         setup_sql=f"UPDATE applications SET watch_paths = 'src/**' WHERE id = {app}")
    sql(f"UPDATE applications SET watch_paths = NULL WHERE id = {app}")
    sql(f"UPDATE application_settings SET is_auto_deploy_enabled = false WHERE application_id = {app}")
    case("github push auto deploy off", url, app, sent("push", push))
    sql(f"UPDATE application_settings SET is_auto_deploy_enabled = true WHERE application_id = {app}")
    case("github pr preview off", url, app, sent("pull_request", pr))
    sql(f"UPDATE application_settings SET is_preview_deployments_enabled = true WHERE application_id = {app}")
    case("github pr opened", url, app, sent("pull_request", pr), want_pr=True)
    case("github pr bad url", url, app, sent("pull_request", {**pr, "pull_request": {**pr["pull_request"], "html_url": "ftp://x"}}))
    case("github pr no action", url, app, sent("pull_request", {**pr, "action": None}))
    fork = json.loads(json.dumps(pr))
    fork["pull_request"]["head"]["repo"] = {"id": 5555, "full_name": "x/r", "fork": True}
    case("github pr from a fork, public previews off", url, app, sent("pull_request", fork), pause=2)
    skip = json.loads(json.dumps(pr))
    skip["pull_request"]["title"] = "x [skip cd]"
    case("github pr skip marker", url, app, sent("pull_request", skip), pause=2)
    # Closing removes the preview.
    closed = {**pr, "action": "closed"}
    case("github pr closed without preview", url, app, sent("pull_request", closed), pause=2)
    sql(f"UPDATE application_settings SET is_preview_deployments_enabled = false WHERE application_id = {app}")


def manual_cases(apps):
    app = apps["mn"]
    full = "acme/manual"

    def gh(event, payload, secret=MANUAL["github"], ctype="application/json"):
        def build(sha):
            p = json.loads(json.dumps(payload).replace("SHA", sha))
            raw = json.dumps(p).encode()
            if ctype != "application/json":
                import urllib.parse
                raw = urllib.parse.urlencode({"payload": raw.decode()}).encode()
            return raw, {"Content-Type": ctype, "X-GitHub-Event": event, "X-Hub-Signature-256": "sha256=" + sign(secret, raw)}
        return build

    def gt(event, payload, secret=MANUAL["gitea"]):
        def build(sha):
            p = json.loads(json.dumps(payload).replace("SHA", sha))
            raw = json.dumps(p).encode()
            return raw, {"Content-Type": "application/json", "X-Gitea-Event": event, "X-Hub-Signature-256": "sha256=" + sign(secret, raw)}
        return build

    def gl(payload, token=MANUAL["gitlab"]):
        def build(sha):
            p = json.loads(json.dumps(payload).replace("SHA", sha))
            return json.dumps(p).encode(), {"Content-Type": "application/json", "X-Gitlab-Token": token}
        return build

    def bb(event, payload, secret=MANUAL["bitbucket"], malformed=False):
        def build(sha):
            p = json.loads(json.dumps(payload).replace("SHA", sha))
            raw = json.dumps(p).encode()
            sig = "nope" if malformed else "sha256=" + sign(secret, raw)
            return raw, {"Content-Type": "application/json", "X-Event-Key": event, "X-Hub-Signature": sig}
        return build

    push = {"ref": "refs/heads/main", "after": "SHA", "repository": {"full_name": full}, "commits": [{"message": "Work", "modified": ["src/a.js"]}]}
    g = "/webhooks/source/github/events/manual"
    case("manual github ping", g, app, gh("ping", {}))
    case("manual github other event", g, app, gh("issues", {"ref": "x"}))
    case("manual github push", g, app, gh("push", push))
    case("manual github form payload", g, app, gh("push", push, ctype="application/x-www-form-urlencoded"))
    case("manual github wrong secret", g, app, gh("push", push, secret="bad"))
    case("manual github unknown repository", g, app, gh("push", {**push, "repository": {"full_name": "no/such"}}))
    case("manual github invalid repository", g, app, gh("push", {**push, "repository": {"full_name": "a b"}}))
    case("manual github no branch", g, app, gh("push", {**push, "ref": ""}))
    case("manual github deleted branch", g, app, gh("push", {**push, "deleted": True}))
    case("manual github skip", g, app, gh("push", {**push, "commits": [{"message": "[skip ci]"}]}))
    case("manual github watch paths", g, app, gh("push", push), setup_sql=f"UPDATE applications SET watch_paths = 'docs/**' WHERE id = {app}")
    sql(f"UPDATE applications SET watch_paths = NULL WHERE id = {app}")
    pr = {"action": "opened", "number": 8, "repository": {"full_name": full},
          "pull_request": {"html_url": "https://example.test/acme/manual/pull/8", "title": "T",
                           "head": {"ref": "feature", "sha": "SHA", "repo": {"id": 1, "full_name": full}},
                           "base": {"ref": "main", "repo": {"id": 1, "full_name": full}}}}
    case("manual github pr preview off", g, app, gh("pull_request", pr))
    sql(f"UPDATE application_settings SET is_preview_deployments_enabled = true WHERE application_id = {app}")
    case("manual github pr opened", g, app, gh("pull_request", pr), want_pr=True)
    case("manual github pr closed, no preview", g, app, gh("pull_request", {**pr, "action": "closed"}), pause=2)

    # GitLab manual.
    l = "/webhooks/source/gitlab/events/manual"
    lpush = {"object_kind": "push", "ref": "refs/heads/main", "after": "SHA", "project": {"id": 1, "path_with_namespace": full}, "commits": [{"message": "w", "added": ["a"]}]}
    mr = {"object_kind": "merge_request", "project": {"id": 1, "path_with_namespace": full},
          "object_attributes": {"action": "open", "iid": 9, "url": "https://example.test/acme/manual/-/merge_requests/9", "title": "T",
                                "source_branch": "feature", "target_branch": "main", "source_project_id": 1, "target_project_id": 1,
                                "last_commit": {"id": "SHA", "message": "m"}}}
    case("manual gitlab bad event", l, app, gl({"object_kind": "tag"}))
    case("manual gitlab no token", l, app, gl(lpush, token=""))
    case("manual gitlab wrong token", l, app, gl(lpush, token="bad"))
    case("manual gitlab push", l, app, gl(lpush))
    case("manual gitlab deleted", l, app, gl({**lpush, "after": "0" * 40}))
    case("manual gitlab bad iid", l, app, gl({**mr, "object_attributes": {**mr["object_attributes"], "iid": "x"}}))
    case("manual gitlab mr open", l, app, gl(mr))
    fork = json.loads(json.dumps(mr))
    fork["object_attributes"]["source_project_id"] = 2
    case("manual gitlab mr from another project", l, app, gl(fork))
    skip = json.loads(json.dumps(mr))
    skip["object_attributes"]["title"] = "T [skip ci]"
    case("manual gitlab mr skip", l, app, gl(skip))
    case("manual gitlab mr close without preview", l, app, gl({**mr, "object_attributes": {**mr["object_attributes"], "action": "close"}}))
    # The preview cleanup runs on a queue upstream and needs the server, so only the answers compare.
    add_preview = f"UPDATE server_settings SET is_reachable = true, is_usable = true WHERE server_id = (SELECT id FROM servers WHERE name = '{PREFIX}-server'); DELETE FROM application_previews WHERE application_id = {app}; INSERT INTO application_previews (uuid, application_id, pull_request_id, pull_request_html_url, git_type, created_at, updated_at) VALUES ('{PREFIX}-pr-' || floor(random() * 1000000), {app}, 9, 'https://example.test', 'gitlab', now(), now())"
    case("manual gitlab mr close with a preview", l, app, gl({**mr, "object_attributes": {**mr["object_attributes"], "action": "close"}}), setup_sql=add_preview, compare_rows=False)
    case("manual gitlab mr unknown action", l, app, gl({**mr, "object_attributes": {**mr["object_attributes"], "action": "approved"}}))

    # Gitea manual.
    t = "/webhooks/source/gitea/events/manual"
    case("manual gitea ping", t, app, gt("ping", {}))
    case("manual gitea other", t, app, gt("issues", {}))
    case("manual gitea push", t, app, gt("push", push))
    case("manual gitea wrong", t, app, gt("push", push, secret="x"))
    gpr = {**pr, "action": "opened"}
    case("manual gitea pr opened", t, app, gt("pull_request", gpr))
    case("manual gitea pr closed no preview", t, app, gt("pull_request", {**gpr, "action": "closed"}))

    # Bitbucket.
    b = "/webhooks/source/bitbucket/events/manual"
    bpush = {"repository": {"full_name": full}, "push": {"changes": [{"new": {"name": "main", "target": {"hash": "SHA"}}, "commits": [{"message": "w"}]}]}}
    bpr = {"repository": {"full_name": full, "uuid": "{r}"},
           "pullrequest": {"id": 10, "title": "T", "links": {"html": {"href": "https://example.test/pr/10"}},
                           "destination": {"branch": {"name": "main"}, "repository": {"uuid": "{r}"}},
                           "source": {"branch": {"name": "feature"}, "commit": {"hash": "SHA"}, "repository": {"uuid": "{r}"}}}}
    case("bitbucket unknown event", b, app, bb("repo:fork", bpush))
    case("bitbucket push", b, app, bb("repo:push", bpush))
    case("bitbucket malformed signature", b, app, bb("repo:push", bpush, malformed=True))
    case("bitbucket wrong secret", b, app, bb("repo:push", bpush, secret="x"))
    case("bitbucket no branch", b, app, bb("repo:push", {**bpush, "push": {"changes": []}}))
    case("bitbucket pr created", b, app, bb("pullrequest:created", bpr))
    case("bitbucket pr rejected, no preview", b, app, bb("pullrequest:rejected", bpr))
    sql(f"UPDATE application_settings SET is_preview_deployments_enabled = false WHERE application_id = {app}")
    case("bitbucket pr created, previews off", b, app, bb("pullrequest:created", bpr))


def oauth_cases(apps):
    app = apps["gl"]
    u = "/webhooks/source/gitlab/events"

    def gl(payload, token=GL_TOKEN, extra=None):
        def build(sha):
            p = json.loads(json.dumps(payload).replace("SHA", sha))
            return json.dumps(p).encode(), {"Content-Type": "application/json", "X-Gitlab-Token": token, **(extra or {})}
        return build

    push = {"object_kind": "push", "ref": "refs/heads/main", "after": "SHA", "project": {"id": 2345}, "commits": [{"message": "w", "added": ["a"]}]}
    mr = {"object_kind": "merge_request", "project": {"id": 2345},
          "object_attributes": {"action": "open", "iid": 11, "url": "https://example.test/o/r/-/merge_requests/11", "title": "T",
                                "source_branch": "feature", "target_branch": "main", "source_project_id": 2345, "target_project_id": 2345,
                                "last_commit": {"id": "SHA", "message": "m"}}}
    case("gitlab bad event", u, app, gl({"object_kind": "x"}))
    case("gitlab no token", u, app, gl(push, token=""))
    case("gitlab wrong token", u, app, gl(push, token="bad"))
    case("gitlab push", u, app, gl(push))
    case("gitlab push no project", u, app, gl({**push, "project": {}}))
    case("gitlab push other project", u, app, gl({**push, "project": {"id": 1}}))
    case("gitlab push no branch", u, app, gl({**push, "ref": None}))
    case("gitlab push deleted", u, app, gl({**push, "after": "0" * 40}))
    case("gitlab push skip", u, app, gl({**push, "commits": [{"message": "[skip cd]"}]}))
    case("gitlab system hook push", u, app, gl({"event_name": "push", "ref": "refs/heads/main", "after": "SHA", "project_id": 2345, "project": {"name": "r"}}, extra={"X-Gitlab-Event": "System Hook"}))
    case("gitlab mr preview off", u, app, gl(mr))
    sql(f"UPDATE application_settings SET is_preview_deployments_enabled = true WHERE application_id = {app}")
    case("gitlab mr open", u, app, gl(mr))
    fork = json.loads(json.dumps(mr))
    fork["object_attributes"]["source_project_id"] = 1
    case("gitlab mr from another project", u, app, gl(fork))
    case("gitlab mr close, no preview", u, app, gl({**mr, "object_attributes": {**mr["object_attributes"], "action": "close"}}))
    sql(f"UPDATE application_settings SET is_preview_deployments_enabled = false WHERE application_id = {app}")

    app = apps["gt"]
    u = "/webhooks/source/gitea/events"

    def gt(event, payload, secret=GT_SECRET, header="X-Gitea-Signature", raw_sig=None):
        def build(sha):
            p = json.loads(json.dumps(payload).replace("SHA", sha))
            raw = json.dumps(p).encode()
            value = raw_sig if raw_sig is not None else sign(secret, raw)
            if header == "X-Hub-Signature-256" and raw_sig is None:
                value = "sha256=" + value
            return raw, {"Content-Type": "application/json", "X-Gitea-Event": event, header: value}
        return build

    tpush = {"ref": "refs/heads/main", "after": "SHA", "repository": {"id": 3456, "full_name": "o/r"}, "commits": [{"message": "w", "added": ["a"]}]}
    tpr = {"action": "opened", "number": 12, "repository": {"id": 3456},
           "pull_request": {"html_url": "https://example.test/o/r/pulls/12", "title": "T",
                            "head": {"ref": "feature", "sha": "SHA", "repo": {"id": 3456}}, "base": {"ref": "main", "repo": {"id": 3456}}}}
    case("gitea ping", u, app, gt("ping", {}))
    case("gitea other", u, app, gt("issues", {}))
    case("gitea no signature", u, app, gt("push", tpush, raw_sig=""))
    case("gitea wrong signature", u, app, gt("push", tpush, secret="x"))
    case("gitea push", u, app, gt("push", tpush))
    case("gitea push hub signature", u, app, gt("push", tpush, header="X-Hub-Signature-256"))
    case("gitea push bad id", u, app, gt("push", {**tpush, "repository": {"id": "x"}}))
    case("gitea push no branch", u, app, gt("push", {**tpush, "ref": ""}))
    case("gitea pr previews off", u, app, gt("pull_request", tpr))
    sql(f"UPDATE application_settings SET is_preview_deployments_enabled = true WHERE application_id = {app}")
    case("gitea pr opened", u, app, gt("pull_request", tpr))
    case("gitea pr closed, no preview", u, app, gt("pull_request", {**tpr, "action": "closed"}))
    sql(f"UPDATE application_settings SET is_preview_deployments_enabled = false WHERE application_id = {app}")


def main():
    teardown()
    apps = setup()
    try:
        github_cases(apps)
        manual_cases(apps)
        oauth_cases(apps)
    finally:
        teardown()
    failed = results.count(False)
    print(f"\n{len(results) - failed}/{len(results)} checks passed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
