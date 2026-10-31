#!/usr/bin/env python3
"""Source rows for the UI comparison, in the team of the test project. Written through upstream's
models (tinker), so both UIs read rows upstream wrote.

    sources_fixture.py up FAKE_BASE     creates them and prints the names and uuids
    sources_fixture.py down             deletes them
"""
import subprocess
import sys

PREFIX = "uisrc"


def php(code):
    out = subprocess.run(
        ["docker", "exec", "coolify-rs-reference-coolify-1", "php", "/var/www/html/artisan", "tinker", "--execute", code],
        capture_output=True, text=True, check=True,
    ).stdout
    return out.strip()


def up(fake):
    team = "App\\Models\\Project::where('uuid','j8xm2emlllhfnyebjzgyzp7z')->first()->team_id"
    print(php(f"""
$t = {team};
$k = App\\Models\\PrivateKey::create(['name' => '{PREFIX}-key', 'team_id' => $t, 'private_key' => App\\Models\\PrivateKey::generateNewKeyPair('rsa')['private_key']]);
$g = fn ($a) => App\\Models\\GithubApp::create($a + ['team_id' => $t, 'api_url' => '{fake}/github', 'html_url' => '{fake}/gh']);
$g1 = $g(['name' => '{PREFIX}-unregistered']);
$g2 = $g(['name' => '{PREFIX}-noinstall', 'app_id' => 4242, 'private_key_id' => $k->id, 'client_id' => 'cid', 'client_secret' => 's', 'webhook_secret' => 'w', 'organization' => 'acme']);
$g3 = $g(['name' => '{PREFIX}-connected', 'app_id' => 4242, 'installation_id' => 99, 'private_key_id' => $k->id, 'client_id' => 'cid', 'client_secret' => 's', 'webhook_secret' => 'w', 'contents' => 'read', 'metadata' => 'read', 'pull_requests' => 'write', 'webhook_events' => ['push', 'pull_request']]);
$o = ['team_id' => $t, 'custom_user' => 'git', 'custom_port' => 22];
$l1 = App\\Models\\GitlabApp::create($o + ['name' => '{PREFIX}-gl-new', 'html_url' => '{fake}/gl', 'api_url' => '{fake}/gitlab/api/v4', 'webhook_token' => 'tok', 'client_id' => 'appid', 'client_secret' => 'sec', 'redirect_uri' => 'http://192.0.2.10:8000/webhooks/source/gitlab/redirect']);
$l2 = App\\Models\\GitlabApp::create($o + ['name' => '{PREFIX}-gl-on', 'html_url' => '{fake}/gl', 'api_url' => '{fake}/gitlab/api/v4', 'webhook_token' => 'tok', 'client_id' => 'appid', 'client_secret' => 'sec', 'access_token' => 'a', 'refresh_token' => 'r', 'expires_at' => time() + 9000, 'group_name' => 'acme']);
$e1 = App\\Models\\GiteaApp::create($o + ['name' => '{PREFIX}-gt-new', 'html_url' => '{fake}/gt', 'api_url' => '{fake}/gitea/api/v1', 'webhook_secret' => 'wh']);
$e2 = App\\Models\\GiteaApp::create($o + ['name' => '{PREFIX}-gt-on', 'html_url' => '{fake}/gt', 'api_url' => '{fake}/gitea/api/v1', 'webhook_secret' => 'wh', 'client_id' => 'cid', 'client_secret' => 'sec', 'access_token' => 'a', 'refresh_token' => 'r', 'expires_at' => time() + 9000, 'organization' => 'acme']);
foreach (['g1' => $g1, 'g2' => $g2, 'g3' => $g3, 'l1' => $l1, 'l2' => $l2, 'e1' => $e1, 'e2' => $e2] as $n => $m) {{ echo $n.'='.$m->uuid.PHP_EOL; }}
"""))


def down():
    print(php(f"""
App\\Models\\GithubApp::where('name', 'like', '{PREFIX}-%')->orWhere('name', 'fake-app')->get()->each(fn ($g) => DB::table('github_apps')->where('id', $g->id)->delete());
App\\Models\\GitlabApp::where('name', 'like', '{PREFIX}-%')->get()->each(fn ($g) => DB::table('gitlab_apps')->where('id', $g->id)->delete());
App\\Models\\GiteaApp::where('name', 'like', '{PREFIX}-%')->get()->each(fn ($g) => DB::table('gitea_apps')->where('id', $g->id)->delete());
App\\Models\\PrivateKey::where('name', '{PREFIX}-key')->orWhere('name', 'github-app-fake-app')->get()->each(fn ($k) => $k->delete());
echo 'removed';
"""))


if __name__ == "__main__":
    up(sys.argv[2]) if sys.argv[1] == "up" else down()
