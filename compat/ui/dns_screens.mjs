// Screens that need a provider answer: secret manager keys after "Browse keys" (application, service),
// and the tunnel conflict. coolify-rs runs its real flow against compat/diff/fake_providers.py.
// The reference must not call a provider from a browser click, so its component runs under tinker with
// Http::fake, and the HTML it renders replaces the component on the real page. That shows upstream's
// markup with upstream's CSS, not a click through upstream's page.
//   node dns_screens.mjs --rs=18087
import fs from 'node:fs';
import { execFileSync } from 'node:child_process';
import { chromium } from 'playwright-core';

const args = process.argv.slice(2);
const rs = (args.find((a) => a.startsWith('--rs=')) ?? '=18087').split('=')[1];
const FAKE_FROM_REF = 'http://host.docker.internal:18191';
const FAKE = 'http://127.0.0.1:18191';
const secret = fs.readFileSync(new URL('../reference/secrets/ui-login.txt', import.meta.url), 'utf8').match(/^password:\s*(.+)$/m)[1].trim();
const out = new URL('./shots/', import.meta.url).pathname;
const sql = (q) => execFileSync('docker', ['exec', 'coolify-rs-reference-postgres-1', 'psql', '-U', 'coolify', '-d', 'coolify', '-At', '-c', q], { encoding: 'utf8' }).trim();
const FAKE_PHP = String.raw`
\Illuminate\Support\Facades\Http::preventStrayRequests();
\Illuminate\Support\Facades\Http::fake(function ($req) {
    $url = $req->url();
    foreach (['https://api.cloudflare.com' => '/cloudflare', 'https://api.doppler.com' => '/doppler'] as $from => $to) {
        if (str_starts_with($url, $from)) { $url = '${FAKE_FROM_REF}'.$to.substr($url, strlen($from)); }
    }
    if (! str_starts_with($url, '${FAKE_FROM_REF}')) { throw new \RuntimeException('stray '.$url); }
    $headers = array_filter(['Authorization' => $req->header('Authorization')[0] ?? null, 'Accept' => 'application/json', 'Content-Type' => $req->header('Content-Type')[0] ?? null]);
    $r = (new \GuzzleHttp\Client(['http_errors' => false]))->request($req->method(), $url, ['headers' => $headers, 'body' => $req->body()]);
    return \Illuminate\Support\Facades\Http::response((string) $r->getBody(), $r->getStatusCode());
});
\Illuminate\Support\Facades\Bus::fake();
\Illuminate\Support\Facades\Auth::login(\App\Models\User::where('email', 'alice@example.test')->first());
session(['currentTeam' => \App\Models\Team::find(TEAM)]);
`;
const tinker = (code) => {
  const text = execFileSync('docker', ['exec', 'coolify-rs-reference-coolify-1', 'php', '/var/www/html/artisan', 'tinker', '--execute', code], { encoding: 'utf8', maxBuffer: 1 << 26 });
  if (!text.includes('<<<')) throw new Error('tinker: ' + text.slice(-400));
  return JSON.parse(text.split('<<<')[1].split('>>>')[0]);
};

const PROJECT = 'j8xm2emlllhfnyebjzgyzp7z';
const ENV = 'uj3st9yvtvzxh7rul3c3opcz';
const tracked = {};
const team = sql(`SELECT team_id FROM servers WHERE uuid = 'hml20fsj5eles0xfiqidq8j5'`);
const prelude = FAKE_PHP.replace('TEAM', team);

function setup() {
  Object.assign(tracked, tinker(String.raw`
$o = \App\Models\Server::where('uuid', 'hml20fsj5eles0xfiqidq8j5')->first();
$e = \App\Models\Environment::where('uuid', '${ENV}')->first();
$tok = \App\Models\IntegrationToken::query()->create(['team_id' => $o->team_id, 'provider' => 'doppler', 'name' => 'Shot Doppler', 'token' => 'dp.st.good', 'capabilities' => ['secrets'], 'metadata' => []]);
$cf = \App\Models\IntegrationToken::query()->create(['team_id' => $o->team_id, 'provider' => 'cloudflare', 'name' => 'Shot Cloudflare', 'token' => 'cf-good', 'capabilities' => ['dns', 'tunnel'], 'metadata' => []]);
$z = \App\Models\DnsProviderZone::query()->create(['integration_token_id' => $cf->id, 'provider_zone_id' => 'zone-example', 'name' => 'example.com', 'account_id' => 'acct-1']);
$a = new \App\Models\Application(['name' => 'shot-app', 'repository_project_id' => 0, 'git_repository' => 'x/y', 'git_branch' => 'main', 'build_pack' => 'dockerimage', 'ports_exposes' => '80', 'environment_id' => $e->id, 'destination_id' => $o->standaloneDockers()->first()->id, 'destination_type' => \App\Models\StandaloneDocker::class, 'docker_registry_image_name' => 'nginx', 'docker_registry_image_tag' => 'alpine']);
$a->save();
$s = new \App\Models\Service(['name' => 'shot-svc', 'environment_id' => $e->id, 'server_id' => $o->id, 'destination_id' => $o->standaloneDockers()->first()->id, 'destination_type' => \App\Models\StandaloneDocker::class, 'docker_compose_raw' => "services:\n  web:\n    image: nginx:alpine\n"]);
$s->save();
$sa = \App\Models\ServiceApplication::query()->create(['name' => 'web', 'service_id' => $s->id, 'image' => 'nginx:alpine']);
\App\Models\SecretManagerLink::query()->create(['resourceable_type' => $a->getMorphClass(), 'resourceable_id' => $a->id, 'integration_token_id' => $tok->id, 'settings' => []]);
\App\Models\SecretManagerLink::query()->create(['resourceable_type' => $s->getMorphClass(), 'resourceable_id' => $s->id, 'integration_token_id' => $tok->id, 'settings' => []]);
$t = \App\Models\Server::create(['name' => 'shot-tunnel', 'ip' => $o->ip, 'user' => $o->user, 'port' => $o->port, 'team_id' => $o->team_id, 'private_key_id' => $o->private_key_id, 'proxy' => $o->proxy]);
$t->settings()->update(['is_reachable' => true, 'is_usable' => true]);
echo '<<<'.json_encode(['tok' => $tok->id, 'cf' => $cf->id, 'zone' => $z->id, 'app' => $a->id, 'app_uuid' => $a->uuid, 'svc' => $s->id, 'svc_uuid' => $s->uuid, 'sub' => $sa->id, 'srv' => $t->id, 'srv_uuid' => $t->uuid]).'>>>';`));
}

function cleanup() {
  const t = tracked;
  if (t.app) {
    sql(`DELETE FROM secret_manager_links WHERE resourceable_type = 'App\\Models\\Application' AND resourceable_id = ${t.app}`);
    sql(`DELETE FROM application_settings WHERE application_id = ${t.app}`);
    sql(`DELETE FROM applications WHERE id = ${t.app}`);
  }
  if (t.svc) {
    sql(`DELETE FROM secret_manager_links WHERE resourceable_type = 'App\\Models\\Service' AND resourceable_id = ${t.svc}`);
    sql(`DELETE FROM service_applications WHERE id = ${t.sub}`);
    sql(`DELETE FROM services WHERE id = ${t.svc}`);
  }
  if (t.cf) {
    sql(`DELETE FROM managed_dns_record_references WHERE managed_dns_record_id IN (SELECT id FROM managed_dns_records WHERE integration_token_id = ${t.cf})`);
    sql(`DELETE FROM managed_dns_records WHERE integration_token_id = ${t.cf}`);
    if (t.srv) sql(`UPDATE server_settings SET cloudflare_tunnel_id = NULL, cloudflare_tunnel_account_id = NULL, cloudflare_tunnel_integration_token_id = NULL WHERE server_id = ${t.srv}`);
    sql(`DELETE FROM dns_provider_zones WHERE id = ${t.zone}`);
    sql(`DELETE FROM integration_tokens WHERE id = ${t.cf}`);
  }
  if (t.tok) sql(`DELETE FROM integration_tokens WHERE id = ${t.tok}`);
  if (t.srv) {
    sql(`DELETE FROM standalone_dockers WHERE server_id = ${t.srv}`);
    sql(`DELETE FROM server_settings WHERE server_id = ${t.srv}`);
    sql(`DELETE FROM servers WHERE id = ${t.srv}`);
  }
}

async function login(browser, base) {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 1000 }, colorScheme: 'dark' });
  // Nothing but the two local instances: a favicon lookup of a row must not leave this machine.
  await ctx.route('**/*', (route) => (/^https?:\/\/127\.0\.0\.1/.test(route.request().url()) || route.request().url().startsWith('data:') ? route.continue() : route.abort()));
  const page = await ctx.newPage();
  await page.goto(base + '/login');
  await page.fill('input[name=email]', 'alice@example.test');
  await page.fill('input[name=password]', secret);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 }).catch(() => {}), page.click('button[type=submit]')]);
  return page;
}

async function replaceComponent(page, text, html) {
  await page.waitForTimeout(800);
  const done = await page.evaluate(([marker, markup]) => {
    const all = [...document.querySelectorAll('[wire\\:id]')].filter((e) => e.textContent.includes(marker));
    const el = all[all.length - 1];
    if (!el) return false;
    el.outerHTML = markup;
    return true;
  }, [text, html]);
  if (!done) throw new Error(`no component with ${text}`);
  await page.waitForTimeout(400);
}

const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
let code = 0;
try {
  setup();
  const ref = await login(browser, 'http://127.0.0.1:18000');
  const mine = await login(browser, `http://127.0.0.1:${rs}`);
  const dismiss = async (page) => {
    for (const label of ['Accept And Close', 'Maybe next time']) await page.locator(`button:has-text("${label}")`).first().click({ timeout: 1500 }).catch(() => {});
  };

  for (const [name, model, id, path] of [
    ['app', 'Application', tracked.app, `/project/${PROJECT}/environment/${ENV}/application/${tracked.app_uuid}/environment-variables`],
    ['svc', 'Service', tracked.svc, `/project/${PROJECT}/environment/${ENV}/service/${tracked.svc_uuid}/environment-variables`],
  ]) {
    const { html } = tinker(prelude + String.raw`
$cls = '\\App\\Models\\${model}';
$t = \Livewire\Livewire::test(\App\Livewire\Project\Shared\SecretManagerLinks::class, ['resource' => $cls::find(${id})])->call('loadKeys');
echo '<<<'.json_encode(['html' => $t->html()]).'>>>';`);
    await ref.goto('http://127.0.0.1:18000' + path);
    await dismiss(ref);
    await replaceComponent(ref, 'Browse keys', html);
    await ref.screenshot({ path: `${out}smkeys-${name}-ref.png`, fullPage: true });
    await mine.goto(`http://127.0.0.1:${rs}${path}`);
    await dismiss(mine);
    await mine.locator('button:has-text("Browse keys")').first().click();
    await mine.locator('button:has-text("Reload keys")').first().waitFor({ timeout: 15000 });
    await mine.waitForTimeout(500);
    await mine.screenshot({ path: `${out}smkeys-${name}-rs.png`, fullPage: true });
  }

  // Domains with every DNS state, on the same rows on both sides. Only .test names.
  const statuses = (urls) => JSON.stringify(Object.fromEntries(urls.map(([u, status, message]) => [u, { status, message, expected_ip: '10.0.0.1', checked_at: '2026-11-01T00:00:00+00:00', check_id: null }]))).replaceAll("'", "''");
  const rows = [['https://ok.shot.example.test', 'ok', 'DNS resolves to the server.'], ['https://bad.shot.example.test', 'failed', 'DNS points elsewhere.'], ['https://skip.shot.example.test', 'skipped', 'Behind a proxy.'], ['https://new.shot.example.test', 'pending', '']];
  const fqdn = rows.map((r) => r[0]).join(',');
  const stored = statuses(rows.filter((r) => r[1] !== 'pending'));
  sql(`UPDATE applications SET fqdn = '${fqdn}', domain_dns_statuses = '${stored}' WHERE id = ${tracked.app}`);
  sql(`UPDATE service_applications SET fqdn = '${fqdn}', domain_dns_statuses = '${stored}' WHERE id = ${tracked.sub}`);
  for (const [name, path] of [
    ['domains-app', `/project/${PROJECT}/environment/${ENV}/application/${tracked.app_uuid}/domains`],
    ['domains-svc', `/project/${PROJECT}/environment/${ENV}/service/${tracked.svc_uuid}/domains`],
  ]) {
    for (const [page, base, side] of [[ref, 'http://127.0.0.1:18000', 'ref'], [mine, `http://127.0.0.1:${rs}`, 'rs']]) {
      await page.goto(base + path);
      await page.waitForTimeout(1500);
      await dismiss(page);
      await page.waitForTimeout(600);
      await page.screenshot({ path: `${out}${name}-${side}.png`, fullPage: true });
    }
  }
  sql(`UPDATE applications SET fqdn = NULL, domain_dns_statuses = NULL WHERE id = ${tracked.app}`);
  sql(`UPDATE service_applications SET fqdn = NULL, domain_dns_statuses = NULL WHERE id = ${tracked.sub}`);

  // The tunnel conflict: the hostname already points elsewhere at the fake.
  const resetFake = async () => {
    await fetch(`${FAKE}/__reset`, { method: 'POST' });
    await fetch(`${FAKE}/cloudflare/client/v4/zones/zone-example/dns_records`, { method: 'POST', headers: { Authorization: 'Bearer cf-good', 'Content-Type': 'application/json' }, body: JSON.stringify({ type: 'A', name: 'ssh.example.com', content: '9.9.9.9' }) });
  };
  await resetFake();
  const { html, conflict } = tinker(prelude + String.raw`
$s = \App\Models\Server::find(${tracked.srv});
$t = \Livewire\Livewire::test(\App\Livewire\Server\CloudflareTunnel::class, ['server_uuid' => $s->uuid])->set('cloudflare_integration_token_id', ${tracked.cf})->set('api_ssh_domain', 'ssh.example.com')->call('apiCloudflareConfig');
echo '<<<'.json_encode(['html' => $t->html(), 'conflict' => $t->get('dnsConflict')]).'>>>';`);
  console.log('reference conflict', JSON.stringify(conflict));
  const tunnel = `/server/${tracked.srv_uuid}/cloudflare-tunnel`;
  await ref.goto('http://127.0.0.1:18000' + tunnel);
  await replaceComponent(ref, 'Set up tunnel', html);
  await ref.screenshot({ path: `${out}tunnelconflict-ref.png`, fullPage: true });
  await resetFake();
  sql(`UPDATE server_settings SET cloudflare_tunnel_id = NULL, cloudflare_tunnel_account_id = NULL, cloudflare_tunnel_integration_token_id = NULL WHERE server_id = ${tracked.srv}`);
  await mine.goto(`http://127.0.0.1:${rs}${tunnel}`);
  await mine.waitForTimeout(800);
  await mine.locator('button:has-text("Select a token")').first().click({ timeout: 2000 }).catch(() => {});
  await mine.locator('[role=option]:has-text("Shot Cloudflare"), .listbox-option:has-text("Shot Cloudflare")').first().click({ timeout: 2000 }).catch(() => {});
  await mine.fill('input[placeholder="ssh.example.com"]', 'ssh.example.com');
  await mine.locator('button:has-text("Set up tunnel")').first().click();
  await mine.locator('text=A DNS record already exists').first().waitFor({ timeout: 15000 });
  await mine.waitForTimeout(500);
  await mine.screenshot({ path: `${out}tunnelconflict-rs.png`, fullPage: true });
} catch (e) {
  console.log('FAILED', String(e.stderr ?? e).replaceAll(secret, '***').slice(0, 300), String(e.stdout ?? '').slice(-700));
  code = 1;
} finally {
  await browser.close();
  cleanup();
}
process.exit(code);
