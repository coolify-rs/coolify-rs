// Drives the cloud provider forms and the Cloud Token page of coolify-rs against a local fake of the
// four provider APIs, and runs the same inputs through upstream's Livewire components inside tinker
// with Http::fake, so neither side reaches a real provider. Compares validation errors, provider
// requests and server rows, and screenshots both sides (the reference's provider step is its
// rendered component HTML, injected into its page).
//   COOLIFY_RS_CLOUD_API_BASE=http://127.0.0.1:18187 scripts/dev-server.sh 18087   (must be running)
//   cd compat/ui && node cloud_ui.mjs [--rs=18087] [--only=hetzner,vultr,digitalocean,hostinger]
// Reference jobs refresh provider state every 5 minutes (ServerManagerJob), and that would reach the
// real API for a row holding a provider id and a token. Each step that creates such a row therefore
// waits for a window far from the tick and deletes the row before it ends.
import { chromium } from 'playwright-core';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';

const args = process.argv.slice(2);
const opt = (k, d) => (args.find((a) => a.startsWith(`--${k}=`)) ?? `=${d}`).split('=')[1];
const rsBase = `http://127.0.0.1:${opt('rs', 18087)}`;
const refBase = 'http://127.0.0.1:18000';
const fakePort = 18187;
const only = opt('only', 'hetzner,vultr,digitalocean,hostinger').split(',');
const out = new URL('./shots/', import.meta.url).pathname;
fs.mkdirSync(out, { recursive: true });
const secret = fs.readFileSync(new URL('../reference/secrets/ui-login.txt', import.meta.url), 'utf8').match(/^password:\s*(.+)$/m)[1].trim();
const REF = 'coolify-rs-reference-coolify-1';
const PG = 'coolify-rs-reference-postgres-1';

const hetznerPage = { meta: { pagination: { next_page: null } } };
const vultrPage = { meta: { links: { next: '' } } };
const FAKE = {
  'GET /hetzner/v1/servers': { servers: [], ...hetznerPage },
  'GET /hetzner/v1/servers/777': { server: { id: 777, status: 'running' } },
  'GET /hetzner/v1/locations': {
    locations: [
      { id: 1, name: 'fsn1', city: 'Falkenstein', country: 'DE', network_zone: 'eu-central' },
      { id: 2, name: 'ash', city: 'Ashburn, VA', country: 'US', network_zone: 'us-east' },
    ],
    ...hetznerPage,
  },
  'GET /hetzner/v1/server_types': {
    server_types: [
      { id: 1, name: 'cx22', description: 'CX22', cores: 2, memory: 4, disk: 40, architecture: 'x86', prices: [{ price_monthly: { gross: '4.5050000000000000' } }], locations: [{ name: 'fsn1' }, { name: 'ash' }] },
      { id: 2, name: 'cax11', description: 'CAX11', cores: 2, memory: 4, disk: 40, architecture: 'arm', prices: [{ price_monthly: { gross: '1234.5000000000000000' } }], locations: [{ name: 'fsn1' }] },
      { id: 3, name: 'old', description: 'Old', cores: 1, memory: 1, disk: 10, architecture: 'x86', deprecated: true, locations: [{ name: 'fsn1' }] },
    ],
    ...hetznerPage,
  },
  'GET /hetzner/v1/images': {
    images: [
      { id: 6, type: 'system', name: 'ubuntu-24.04', description: 'Ubuntu 24.04', architecture: 'x86' },
      { id: 5, type: 'system', name: 'debian-12', description: 'Debian 12', architecture: 'x86' },
      { id: 7, type: 'system', name: 'ubuntu-24.04-arm', description: 'Ubuntu 24.04', architecture: 'arm' },
      { id: 8, type: 'snapshot', name: 'snap', architecture: 'x86' },
      { id: 9, type: 'system', name: 'centos', deprecated: true, architecture: 'x86' },
      { id: 10, name: 'untyped', architecture: 'x86' },
    ],
    ...hetznerPage,
  },
  'GET /hetzner/v1/ssh_keys': { ssh_keys: [{ id: 21, name: 'laptop', fingerprint: 'aa' }, { id: 22, name: 'ci', fingerprint: 'bb' }], ...hetznerPage },
  'POST /hetzner/v1/ssh_keys': { ssh_key: { id: 23 } },
  'GET /hetzner/v1/firewalls': { firewalls: [{ id: 32, name: 'web' }, { id: 31, name: 'db' }], ...hetznerPage },
  'GET /hetzner/v1/networks': {
    networks: [
      { id: 42, name: 'eu', ip_range: '10.0.0.0/16', subnets: [{ type: 'cloud', network_zone: 'eu-central' }] },
      { id: 41, name: 'us', ip_range: '10.1.0.0/16', subnets: [{ type: 'server', network_zone: 'us-east' }] },
      { id: 43, name: 'vswitch', ip_range: '10.2.0.0/16', subnets: [{ type: 'vswitch', network_zone: 'eu-central' }] },
    ],
    ...hetznerPage,
  },
  'POST /hetzner/v1/servers': { server: { id: 777, status: 'initializing', public_net: { ipv4: { ip: '198.51.100.91' }, ipv6: { ip: '2001:db8::/64' } } } },
  'POST /hetzner/v1/servers/777/actions/enable_backup': { action: { id: 1 } },
  'GET /vultr/v2/account': { account: {} },
  'GET /vultr/v2/instances/vi-1': { instance: { id: 'vi-1', status: 'active', main_ip: '198.51.100.92', v6_main_ip: '' } },
  'GET /vultr/v2/regions': { regions: [{ id: 'fra', city: 'Frankfurt', country: 'DE' }, { id: 'ams', city: 'Amsterdam', country: 'NL' }], ...vultrPage },
  'GET /vultr/v2/plans': {
    plans: [
      { id: 'vc2-2c', vcpu_count: 2, ram: 4096, disk: 80, monthly_cost: 20, locations: ['ams'] },
      { id: 'vc2-1c', vcpu_count: 1, ram: 1024, disk: 25, monthly_cost: 5, locations: ['ams', 'fra'] },
      { id: 'vc2-any', vcpu_count: 8, ram: 32768, disk: 400, monthly_cost: 1200.5, locations: [] },
    ],
    ...vultrPage,
  },
  'GET /vultr/v2/os': { os: [{ id: 2, name: 'Ubuntu 24.04' }, { id: 1, name: 'Debian 12' }], ...vultrPage },
  'GET /vultr/v2/ssh-keys': { ssh_keys: [{ id: 'k1', name: 'laptop', ssh_key: 'ssh-rsa AAA x' }], ...vultrPage },
  'POST /vultr/v2/ssh-keys': { ssh_key: { id: 'k2' } },
  'POST /vultr/v2/instances': { instance: { id: 'vi-1', status: 'pending', main_ip: '198.51.100.92', v6_main_ip: '' } },
  'GET /digitalocean/v2/account': { account: {} },
  'GET /digitalocean/v2/droplets/888': { droplet: { id: 888 } },
  'GET /digitalocean/v2/regions': { regions: [{ slug: 'fra1', name: 'Frankfurt 1', available: true }, { slug: 'nyc1', name: 'New York 1', available: true }, { slug: 'off', name: 'Off', available: false }], links: {} },
  'GET /digitalocean/v2/sizes': {
    sizes: [
      { slug: 's-1vcpu-1gb', memory: 1024, vcpus: 1, disk: 25, price_monthly: 6, regions: ['fra1', 'nyc1'] },
      { slug: 's-2vcpu-2gb', memory: 2048, vcpus: 2, disk: 60, price_monthly: 18, regions: ['nyc1'] },
    ],
    links: {},
  },
  'GET /digitalocean/v2/images': {
    images: [
      { id: 2, slug: 'ubuntu-24-04-x64', distribution: 'Ubuntu', name: '24.04 x64', regions: ['fra1', 'nyc1'], public: true },
      { id: 1, slug: null, distribution: 'Debian', name: '12 x64', regions: ['fra1'], public: true },
      { id: 3, slug: 'priv', distribution: 'Mine', name: 'private', regions: ['fra1'], public: false },
    ],
    links: {},
  },
  'GET /digitalocean/v2/account/keys': { ssh_keys: [{ id: 51, name: 'laptop', fingerprint: 'cc' }], links: {} },
  'POST /digitalocean/v2/account/keys': { ssh_key: { id: 52 } },
  'POST /digitalocean/v2/droplets': { droplet: { id: 888, status: 'new', networks: { v4: [{ type: 'public', ip_address: '198.51.100.93' }], v6: [] } } },
  'GET /hostinger/api/vps/v1/data-centers': [{ id: 1, name: 'fra', city: 'Frankfurt', location: 'de' }, { id: 2, name: 'bos', location: '' }],
  'GET /hostinger/api/vps/v1/templates': [
    { id: 4, name: 'Ubuntu 24.04 with Coolify' },
    { id: 2, name: 'Ubuntu 24.04 LTS' },
    { id: 3, name: 'Debian 12' },
  ],
  'GET /hostinger/api/billing/v1/catalog': [
    { id: 'hostingercom-vps-kvm4', name: 'KVM 4', prices: [{ id: 'kvm4-12', currency: 'usd', price: 12000, first_period_price: 9000, period: 12, period_unit: 'month' }] },
    { id: 'hostingercom-vps-kvm2', name: 'KVM 2', prices: [{ id: 'kvm2-24', currency: 'usd', price: 16000, first_period_price: 12000, period: 24, period_unit: 'month' }, { id: 'kvm2-1', currency: 'usd', price: 1000, period: 1, period_unit: 'month' }] },
    { id: 'other', name: 'Other', prices: [{ id: 'x' }] },
  ],
  'GET /hostinger/api/vps/v1/public-keys': { data: [{ id: 61, name: 'laptop' }] },
  'GET /hostinger/api/vps/v1/post-install-scripts': { data: [{ id: 71, name: 'prep' }] },
  'GET /hostinger/api/vps/v1/virtual-machines': [],
  'GET /hostinger/api/vps/v1/virtual-machines/999': { id: 999, state: 'running', ipv4: [{ address: '198.51.100.94' }], ipv6: [] },
  'POST /hostinger/api/vps/v1/virtual-machines': { virtual_machine: { id: 999, state: 'initial', ipv4: [{ address: '198.51.100.94' }], ipv6: [] } },
  'POST /hostinger/api/vps/v1/public-keys/attach/999': { ok: true },
};
const TOKEN_ROUTES = ['/hetzner/v1/servers', '/vultr/v2/account', '/digitalocean/v2/account', '/hostinger/api/vps/v1/virtual-machines'];
// `bad-*`: rejected everywhere. `fail-*`: passes the token check, then the lists fail with the provider's error shape.
const ERRORS = { hetzner: { error: { message: 'unauthorized' } }, vultr: { error: 'Invalid API token.' }, digitalocean: { message: 'Unable to authenticate you.' }, hostinger: { message: 'Unauthenticated.' } };

let log = [];
const fake = http.createServer((req, res) => {
  let body = '';
  req.on('data', (c) => (body += c));
  req.on('end', () => {
    const [path, query] = req.url.split('?');
    const auth = req.headers.authorization ?? '';
    log.push({ method: req.method, path, query: query ?? '', auth, body: body ? JSON.parse(body) : null });
    const provider = path.split('/')[1];
    let status = 200;
    let data = FAKE[`${req.method} ${path}`];
    if (auth.includes('bad-') || (auth.includes('fail-') && !TOKEN_ROUTES.includes(path))) {
      status = 401;
      data = ERRORS[provider];
    } else if (data === undefined) {
      status = 404;
      data = { error: { message: 'unknown' } };
    }
    res.writeHead(status, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify(data));
  });
});
await new Promise((r) => fake.listen(fakePort, '127.0.0.1', r));

const sql = (q) =>
  execFileSync('docker', ['exec', PG, 'psql', '-U', 'coolify', '-d', 'coolify', '-At', '-c', q], { encoding: 'utf8' }).trim();
function deleteServer(uuid) {
  const id = sql(`select id from servers where uuid='${uuid}'`);
  if (!id) return;
  for (const t of ['docker_cleanup_executions', 'shared_environment_variables', 'standalone_dockers', 'swarm_dockers', 'ssl_certificates', 'server_settings']) {
    sql(`delete from ${t} where server_id=${id}`);
  }
  sql(`delete from servers where id=${id}`);
}

/** Waits until at least `need` seconds remain before the next 5-minute tick. */
async function safeWindow(need = 150) {
  const left = 300 - (Math.floor(Date.now() / 1000) % 300);
  if (left >= need) return;
  console.log(`waiting ${left + 15}s for the 5-minute tick to pass`);
  await new Promise((r) => setTimeout(r, (left + 15) * 1000));
}

const results = [];
const check = (name, ok, detail = '') => {
  results.push(ok);
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${ok || !detail ? '' : `\n     ${detail}`}`);
};
const same = (name, a, b) => check(name, JSON.stringify(a) === JSON.stringify(b), `rs  ${JSON.stringify(a)}\n     ref ${JSON.stringify(b)}`);

const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
async function session(base) {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: 'dark' });
  const page = await ctx.newPage();
  await page.addInitScript(() => {
    try {
      localStorage.setItem('theme', 'dark');
    } catch {}
  });
  await page.goto(`${base}/login`);
  await page.fill('input[name=email]', 'alice@example.test');
  await page.fill('input[name=password]', secret);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 }).catch(() => {}), page.click('button[type=submit]')]);
  return page;
}
const rs = await session(rsBase);
const ref = await session(refBase);
const shot = (page, name, side) => page.screenshot({ path: `${out}${name}-${side}.png`, fullPage: true });
async function shotRef(name, path, prepare) {
  await ref.goto(refBase + path);
  await ref.waitForTimeout(1500);
  await ref.locator('button:has-text("Accept And Close"), button:has-text("Accept and Close")').first().click({ timeout: 500 }).catch(() => {});
  if (prepare) await prepare();
  await ref.waitForTimeout(400);
  await shot(ref, name, 'ref');
}

async function choose(page, id, label) {
  await page.click(`[id="${id}-trigger"]`);
  await page.locator('.listbox-panel .listbox-option', { hasText: label }).first().click();
}
const optionsOf = async (page, id) => {
  await page.click(`[id="${id}-trigger"]`);
  await page.waitForTimeout(150);
  const texts = await page.locator('.listbox-panel .listbox-option').allTextContents();
  await page.keyboard.press('Escape');
  await page.click('h1');
  return texts.map((t) => t.replace(/\s+/g, ' ').trim());
};
const errorsOf = (page, ids) =>
  page.evaluate(
    (fields) => Object.fromEntries(fields.map((f) => [f, [...document.querySelectorAll('.text-red-500.label-text-alt')].find((e) => (e.closest('.w-full, div')?.parentElement ?? document).querySelector(`[id="${f}"], [id="${f}-trigger"]`))?.innerText ?? null])),
    ids,
  );
const toastText = async (page) => (await page.locator('[role=status], .toast, [data-toast], li').allInnerTexts()).join(' | ');

const CASES = {
  hetzner: {
    route: 'hetzner',
    label: 'Hetzner',
    cls: 'App\\Livewire\\Server\\New\\ByHetzner',
    load: 'loadHetznerData',
    optionIds: ['selected_location', 'selected_server_type', 'selected_image'],
    sets: [
      ['server_name', 'Diff-Hetzner'],
      ['selected_location', 'fsn1'],
      ['selected_server_type', 'cx22'],
      ['selected_image', 6],
      ['selectedHetznerSshKeyIds', [21]],
      ['selectedHetznerFirewallIds', [31, 32]],
      ['selectedHetznerNetworkIds', [42]],
      ['enable_backups', true],
      ['show_cloud_init_script', true],
      ['cloud_init_script', '#!/bin/sh\necho hetzner'],
      ['save_cloud_init_script', true],
      ['cloud_init_script_name', 'diff-script'],
    ],
    fill: async (p) => {
      await p.fill('#server_name', 'Diff-Hetzner');
      await choose(p, 'selected_location', 'Falkenstein');
      await choose(p, 'selected_server_type', 'CX22');
      await choose(p, 'selected_image', 'Ubuntu 24.04');
      for (const [id, l] of [['selectedHetznerSshKeyIds', 'laptop'], ['selectedHetznerFirewallIds', 'db'], ['selectedHetznerFirewallIds', 'web'], ['selectedHetznerNetworkIds', 'eu']]) {
        await choose(p, id, l);
        await p.keyboard.press('Escape');
        await p.click('h1');
      }
      await p.locator('#enable_backups').check();
      await p.click('button:has-text("Add cloud-init script")');
      await p.fill('#cloud_init_script', '#!/bin/sh\necho hetzner');
      await p.locator('#save_cloud_init_script').check();
      await p.fill('#cloud_init_script_name', 'diff-script');
    },
    columns: 'hetzner_server_id, hetzner_server_status',
  },
  vultr: {
    route: 'vultr',
    label: 'Vultr',
    cls: 'App\\Livewire\\Server\\New\\ByVultr',
    load: 'loadVultrData',
    optionIds: ['selected_region', 'selected_plan', 'selected_os_id'],
    sets: [
      ['server_name', 'Diff-Vultr'],
      ['selected_region', 'ams'],
      ['selected_plan', 'vc2-2c'],
      ['selected_os_id', 2],
      ['selectedVultrSshKeyIds', ['k1']],
      ['cloud_init_script', '#!/bin/sh\necho hi'],
    ],
    fill: async (p) => {
      await p.fill('#server_name', 'Diff-Vultr');
      await choose(p, 'selected_region', 'Amsterdam');
      await choose(p, 'selected_plan', 'vc2-2c');
      await choose(p, 'selected_os_id', 'Ubuntu');
      await choose(p, 'selectedVultrSshKeyIds', 'laptop');
      await p.keyboard.press('Escape');
      await p.click('h1');
      await p.fill('#cloud_init_script', '#!/bin/sh\necho hi');
    },
    columns: 'vultr_instance_id, vultr_instance_status',
  },
  digitalocean: {
    route: 'digital-ocean',
    label: 'DigitalOcean',
    cls: 'App\\Livewire\\Server\\New\\ByDigitalOcean',
    load: 'loadDigitalOceanData',
    optionIds: ['selected_region', 'selected_size', 'selected_image'],
    sets: [
      ['server_name', 'Diff-Do'],
      ['selected_region', 'fra1'],
      ['selected_size', 's-1vcpu-1gb'],
      ['selected_image', 1],
      ['selectedDigitalOceanSshKeyIds', [51]],
      ['monitoring', false],
    ],
    fill: async (p) => {
      await p.fill('#server_name', 'Diff-Do');
      await choose(p, 'selected_region', 'Frankfurt');
      await choose(p, 'selected_size', 's-1vcpu');
      await choose(p, 'selected_image', 'Debian');
      await choose(p, 'selectedDigitalOceanSshKeyIds', 'laptop');
      await p.keyboard.press('Escape');
      await p.click('h1');
      await p.locator('#monitoring').uncheck();
    },
    columns: 'digitalocean_droplet_id, digitalocean_droplet_status',
  },
  hostinger: {
    route: 'hostinger',
    label: 'Hostinger',
    cls: 'App\\Livewire\\Server\\New\\ByHostinger',
    load: 'loadHostingerData',
    optionIds: ['selected_data_center_id', 'selected_price_id', 'selected_os_template_id', 'selected_app_template_id'],
    sets: [
      ['server_name', 'diff.hostinger.test'],
      ['selected_data_center_id', 1],
      ['selected_template_id', 2],
      ['selected_price_id', 'kvm2-1'],
      ['selected_public_key_ids', [61]],
      ['selected_post_install_script_id', 71],
      ['enable_backups', true],
    ],
    fill: async (p) => {
      await p.fill('#server_name', 'diff.hostinger.test');
      await choose(p, 'selected_data_center_id', 'Frankfurt');
      await choose(p, 'selected_price_id', 'KVM 2');
      await choose(p, 'selected_public_key_ids', 'laptop');
      await p.keyboard.press('Escape');
      await p.click('h1');
      await choose(p, 'selected_post_install_script_id', 'prep');
      await p.locator('#enable_backups').check();
    },
    columns: 'hostinger_virtual_machine_id, hostinger_virtual_machine_status',
  },
};

const row = (uuid) =>
  sql(`select (to_jsonb(s) - 'id' - 'uuid' - 'created_at' - 'updated_at' - 'sentinel_updated_at' - 'validation_logs')::text from servers s where s.uuid='${uuid}'`);
const settings = (uuid) => JSON.parse(sql(`select (to_jsonb(s) - 'id' - 'server_id' - 'sentinel_token' - 'sentinel_custom_url' - 'created_at' - 'updated_at')::text from server_settings s where s.server_id=(select id from servers where uuid='${uuid}')`) || 'null');
const norm = (reqs) => reqs.map(({ method, path, query, auth, body }) => ({ method, path, query, auth, body }));

/** Upstream's component inside tinker: Http::fake, same inputs. */
function livewire(provider, c, tokenUuid, failUuid, teamId, serverUuid) {
  const php = `<?php
use Illuminate\\Support\\Facades\\Http;
$fake = json_decode(file_get_contents('/tmp/cloud_ui_fake.json'), true);
$hosts = ['https://api.hetzner.cloud/v1'=>'/hetzner/v1','https://api.vultr.com/v2'=>'/vultr/v2','https://api.digitalocean.com/v2'=>'/digitalocean/v2','https://developers.hostinger.com'=>'/hostinger'];
$tokenRoutes = ${JSON.stringify(TOKEN_ROUTES)};
$errors = json_decode('${JSON.stringify(ERRORS)}', true);
$log = [];
Http::fake(function ($request) use (&$log, $fake, $hosts, $tokenRoutes, $errors) {
    $url = $request->url(); $prefix = ''; $path = parse_url($url, PHP_URL_PATH);
    foreach ($hosts as $h => $p) { if (str_starts_with($url, $h)) { $prefix = $p; $path = substr(explode('?', $url)[0], strlen($h)); } }
    $auth = $request->header('Authorization')[0] ?? '';
    $body = $request->body();
    $log[] = ['method'=>$request->method(),'path'=>$prefix.$path,'query'=>(string) parse_url($url, PHP_URL_QUERY),'auth'=>$auth,'body'=>$body === '' ? null : json_decode($body, true)];
    $full = $prefix.$path;
    if (str_contains($auth, 'bad-') || (str_contains($auth, 'fail-') && ! in_array($full, $tokenRoutes))) { return Http::response($errors[explode('/', $full)[1]], 401); }
    $key = $request->method().' '.$full;
    return isset($fake[$key]) ? Http::response($fake[$key], 200) : Http::response(['error'=>['message'=>'unknown']], 404);
});
$user = App\\Models\\User::where('email', 'alice@example.test')->first();
auth()->login($user);
session(['currentTeam' => App\\Models\\Team::find(${teamId})]);
$out = [];
$props = ['private_keys'=>collect(),'limit_reached'=>false,'selectedTokenUuid'=>'${tokenUuid}'];
$dispatches = fn ($t) => collect($t->effects['dispatches'] ?? [])->map(fn ($d) => [$d['name'], $d['params'][0] ?? null])->all();
${failUuid ? `$t = Livewire\\Livewire::test(${JSON.stringify(c.cls).slice(1, -1).replace(/\\\\/g, '\\')}::class, array_merge($props, ['selectedTokenUuid'=>'${failUuid}']));
$t->call('${c.load}');
$out['error_html'] = $t->html();
$out['error_message'] = $t->get('provider_data_error');
$out['error_toasts'] = $dispatches($t);
$t = Livewire\\Livewire::test(${JSON.stringify(c.cls).slice(1, -1).replace(/\\\\/g, '\\')}::class, array_merge($props, ['selectedTokenUuid'=>'${failUuid}']));
$t->set('private_key_id', App\\Models\\PrivateKey::ownedAndOnlySShKeys()->where('id', '!=', 0)->first()->id);
foreach (${phpValue(c.sets)} as [$k, $v]) { $t->set($k, $v); }
$t->call('submit');
$out['submit_error'] = collect($dispatches($t))->last()[1] ?? null;` : ''}
$t = Livewire\\Livewire::test(${JSON.stringify(c.cls).slice(1, -1).replace(/\\\\/g, '\\')}::class, $props);
$t->call('${c.load}');
$out['form_html'] = $t->html();
$t->set('server_name', 'bad name!');
$t->call('submit');
$out['errors'] = $t->errors()->toArray();
$out['empty_html'] = $t->html();
$log = [];
foreach (${phpValue(c.sets)} as [$k, $v]) { $t->set($k, $v); }
$out['filled_html'] = $t->html();
$t->call('submit');
$out['requests'] = $log;
$out['toasts'] = $dispatches($t);
$out['after_errors'] = $t->errors()->toArray();
$server = App\\Models\\Server::where('team_id', ${teamId})->where('${c.columns.split(',')[0]}', '!=', null)->latest('id')->first();
$out['uuid'] = $server?->uuid;
echo '@@@'.json_encode($out).'@@@';
`;
  return runPhp(php);
}
function phpValue(v) {
  if (Array.isArray(v)) return `[${v.map(phpValue).join(',')}]`;
  if (typeof v === 'string') return `'${v.replace(/\\/g, '\\\\').replace(/'/g, "\\'").replace(/\n/g, "'.\"\\n\".'")}'`;
  return JSON.stringify(v);
}
function runPhp(php) {
  const dir = fs.mkdtempSync('/tmp/cloud-ui-');
  fs.writeFileSync(`${dir}/fake.json`, JSON.stringify(FAKE));
  fs.writeFileSync(`${dir}/run.php`, php);
  execFileSync('docker', ['cp', `${dir}/fake.json`, `${REF}:/tmp/cloud_ui_fake.json`]);
  execFileSync('docker', ['cp', `${dir}/run.php`, `${REF}:/tmp/cloud_ui_run.php`]);
  fs.rmSync(dir, { recursive: true });
  const res = execFileSync('docker', ['exec', REF, 'php', '/var/www/html/artisan', 'tinker', '--execute', "include '/tmp/cloud_ui_run.php';"], { encoding: 'utf8', maxBuffer: 1 << 28 });
  const m = res.match(/@@@(.*)@@@/s);
  if (!m) throw new Error(`tinker: ${res.slice(0, 2000)}`);
  return JSON.parse(m[1]);
}

/** The reference's provider step: its rendered component in its own page, on the token picker's path. */
async function injectRef(route, html, name) {
  await shotRef(name, `/servers/new/${route}`, async () => {
    await ref.evaluate((h) => {
      const host = document.querySelector('.application-settings-form');
      host.innerHTML = h;
    }, html);
    await ref.waitForTimeout(800);
  });
}

const madeTokens = [];
const madeServers = [];
try {
  await rs.goto(`${rsBase}/servers/new`);
  await rs.waitForTimeout(800);
  await shot(rs, 'cloud-landing', 'rs');
  await shotRef('cloud-landing', '/servers/new');
  await rs.goto(`${rsBase}/server/hml20fsj5eles0xfiqidq8j5/cloud-provider-token`);
  await rs.waitForSelector('text=No cloud provider integration');
  await rs.waitForTimeout(400);
  await shot(rs, 'cloud-unmanaged', 'rs');
  await shotRef('cloud-unmanaged', '/server/hml20fsj5eles0xfiqidq8j5/cloud-provider-token');
  for (const provider of only) {
    const c = CASES[provider];
    console.log(`\n== ${c.label}`);
    // The token picker, empty.
    await rs.goto(`${rsBase}/servers/new/${c.route}`);
    await rs.waitForSelector('.empty-state', { timeout: 10000 }).catch((e) => {
      // The database is shared: another run's token hides the empty state.
      console.log(sql('select provider, name, team_id from cloud_provider_tokens'));
      throw e;
    });
    await rs.waitForTimeout(400);
    await shot(rs, `cloud-${provider}-empty`, 'rs');
    await shotRef(`cloud-${provider}-empty`, `/servers/new/${c.route}`);
    // The add-token modal on both sides (the reference's is only opened).
    await rs.click('button:has-text("Add token")');
    await rs.waitForTimeout(300);
    await shot(rs, `cloud-${provider}-modal`, 'rs');
    await shotRef(`cloud-${provider}-modal`, `/servers/new/${c.route}`, () => ref.click('button:has-text("Add token")'));
    // A rejected token, then a good one.
    log = [];
    await rs.fill('#name', `bad ${provider}`);
    await rs.fill('#token', `bad-${provider}`);
    await rs.fill('#description', 'diff token');
    await rs.click('button:has-text("Validate and add")');
    await rs.waitForTimeout(800);
    check('rejected token toast', (await toastText(rs)).includes('Invalid API token. Please check your token and try again.'));
    check('rejected token stored nothing', sql(`select count(*) from cloud_provider_tokens where name='bad ${provider}'`) === '0');
    await rs.fill('#name', `diff ${provider}`);
    await rs.fill('#token', `good-${provider}`);
    await rs.click('button:has-text("Validate and add")');
    await rs.waitForURL(new RegExp(`/servers/new/${c.route}/`), { timeout: 15000 });
    const tokenUuid = rs.url().split('/').pop();
    madeTokens.push(tokenUuid);
    check('good token stored with description', sql(`select description from cloud_provider_tokens where uuid='${tokenUuid}'`) === 'diff token');
    const teamId = sql(`select team_id from cloud_provider_tokens where uuid='${tokenUuid}'`);
    const enc = sql(`select token from cloud_provider_tokens where uuid='${tokenUuid}'`);
    const failUuid = `fail${tokenUuid}`.slice(0, 24);
    sql(`insert into cloud_provider_tokens (team_id, provider, token, name, uuid, created_at, updated_at) values (${teamId}, '${provider}', '${enc}', 'x', '${failUuid}', now(), now())`);
    madeTokens.push(failUuid);
    // A failing token: encrypted by upstream so both sides read it.
    const failEnc = runPhp(`<?php echo '@@@'.json_encode(['v'=>Illuminate\\Support\\Facades\\Crypt::encryptString('fail-${provider}')]).'@@@';`).v;
    sql(`update cloud_provider_tokens set token='${failEnc}', name='failing ${provider}' where uuid='${failUuid}'`);

    // The picker with tokens, both sides.
    await rs.goto(`${rsBase}/servers/new/${c.route}`);
    await rs.waitForSelector(`a[href$="${tokenUuid}"]`);
    await rs.waitForTimeout(300);
    await shot(rs, `cloud-${provider}-picker`, 'rs');
    await shotRef(`cloud-${provider}-picker`, `/servers/new/${c.route}`);

    // Provider error.
    await rs.goto(`${rsBase}/servers/new/${c.route}/${failUuid}`);
    await rs.waitForSelector('text=Provider request failed');
    await rs.waitForTimeout(400);
    const rsError = await rs.locator('pre').innerText();
    await shot(rs, `cloud-${provider}-error`, 'rs');

    // Validation, with the empty form.
    await rs.goto(`${rsBase}/servers/new/${c.route}/${tokenUuid}`);
    await rs.waitForSelector('button:has-text("Buy and create")');
    await rs.waitForTimeout(400);
    await shot(rs, `cloud-${provider}-form`, 'rs');
    await rs.fill('#server_name', 'bad name!');
    await rs.click('button:has-text("Buy and create")');
    await rs.waitForTimeout(800);
    await shot(rs, `cloud-${provider}-invalid`, 'rs');
    const rsErrors = await rs.evaluate(() => [...document.querySelectorAll('.text-red-500.label-text-alt')].map((e) => e.innerText));

    // A provider rejecting the submit: the toast.
    await safeWindow();
    await rs.goto(`${rsBase}/servers/new/${c.route}/${tokenUuid}`);
    await rs.waitForSelector('button:has-text("Buy and create")');
    await c.fill(rs);
    sql(`update cloud_provider_tokens set token='${failEnc}' where uuid='${tokenUuid}'`);
    await rs.click('button:has-text("Buy and create")');
    await rs.waitForSelector('text=Error', { timeout: 30000 });
    await rs.waitForTimeout(300);
    const rsSubmitError = await rs.evaluate(() => [...document.querySelectorAll('*')].find((e) => e.children.length === 0 && e.textContent.trim() === 'Error')?.parentElement?.innerText?.replace(/^Error\s*/, ''));
    await shot(rs, `cloud-${provider}-rejected`, 'rs');
    sql(`update cloud_provider_tokens set token='${enc}' where uuid='${tokenUuid}'`);

    await safeWindow();
    // coolify-rs: fill, submit.
    const rsOptions = {};
    log = [];
    await rs.goto(`${rsBase}/servers/new/${c.route}/${tokenUuid}`);
    await rs.waitForSelector('button:has-text("Buy and create")');
    await c.fill(rs);
    for (const id of c.optionIds) {
      if (id === 'selected_os_template_id' || id === 'selected_app_template_id') continue;
      if (await rs.locator(`[id="${id}-trigger"]:disabled`).count()) continue;
      rsOptions[id] = await optionsOf(rs, id);
    }
    await rs.waitForTimeout(200);
    await shot(rs, `cloud-${provider}-filled`, 'rs');
    log = [];
    await rs.click('button:has-text("Buy and create")');
    await rs.waitForURL(/\/server\/[a-z0-9]+$/, { timeout: 60000 });
    const rsUuid = rs.url().split('/').pop();
    madeServers.push(rsUuid);
    const rsRow = JSON.parse(row(rsUuid));
    const rsSettings = settings(rsUuid);
    const rsRequests = norm(log);
    // The server page polls the provider through the fake from here on: delete at once.
    await rs.goto(`${rsBase}/servers`);

    // Cloud Token page on both sides, then its actions on coolify-rs.
    await rs.goto(`${rsBase}/server/${rsUuid}/cloud-provider-token`);
    await rs.waitForSelector('text=Validate token');
    await rs.waitForTimeout(500);
    await shot(rs, `cloud-${provider}-token-page`, 'rs');
    await shotRef(`cloud-${provider}-token-page`, `/server/${rsUuid}/cloud-provider-token`);
    log = [];
    await rs.click('button:has-text("Validate token")');
    await rs.waitForTimeout(500);
    check('validate token toast', (await toastText(rs)).includes(`${c.label} token is valid and working.`));
    const validateRequests = norm(log);
    sql(`update cloud_provider_tokens set name='failing ${provider}' where uuid='${failUuid}'`);
    log = [];
    await rs.goto(`${rsBase}/server/${rsUuid}/cloud-provider-token`);
    await rs.waitForSelector('button:has-text("Use this token")');
    await rs.locator('button:has-text("Use this token")').first().click();
    await rs.waitForTimeout(800);
    const useRequests = norm(log);
    check('use token: the failing token passes its account check, the server check decides', useRequests.length >= 1);
    await shot(rs, `cloud-${provider}-token-used`, 'rs');
    deleteServer(rsUuid);

    if (provider === 'hetzner') {
      // The saved script: stored, offered, loaded and cleared.
      const scripts = sql("select count(*) from cloud_init_scripts where name='diff-script'");
      check('saved script stored', Number(scripts) >= 1, scripts);
      await rs.goto(`${rsBase}/servers/new/${c.route}/${tokenUuid}`);
      await rs.waitForSelector('button:has-text("Buy and create")');
      await rs.click('button:has-text("Add cloud-init script")');
      await choose(rs, 'selected_cloud_init_script_id', 'diff-script');
      await rs.waitForTimeout(200);
      same('saved script loaded', await rs.inputValue('#cloud_init_script'), '#!/bin/sh\necho hetzner');
      await rs.click('button:has-text("Clear")');
      await rs.waitForSelector('button:has-text("Add cloud-init script")');
      check('clear hides the script', (await rs.locator('#cloud_init_script').count()) === 0);
    }

    // Upstream's component, same inputs.
    await safeWindow();
    const r = livewire(provider, c, tokenUuid, failUuid, teamId);
    if (r.uuid) madeServers.push(r.uuid);
    const refRow = r.uuid ? JSON.parse(row(r.uuid)) : null;
    const refSettings = r.uuid ? settings(r.uuid) : null;
    if (r.uuid) deleteServer(r.uuid);

    same('provider error message', rsError, r.error_message);
    same('validation messages', [...rsErrors].sort(), Object.entries(r.errors).filter(([k]) => k !== 'selected_template_id').map(([, v]) => v[0]).sort());
    // Every option label, in its order, is in upstream's rendered component.
    const decoded = r.filled_html.replace(/&quot;/g, '"').replace(/&#039;/g, "'").replace(/&amp;/g, '&').replace(/\\u([0-9a-f]{4})/g, (_, h) => String.fromCharCode(parseInt(h, 16))).replace(/\\\//g, '/');
    for (const [id, labels] of Object.entries(rsOptions)) {
      let at = 0;
      const missing = labels.filter((l) => {
        const i = decoded.indexOf(l, at);
        if (i < 0) return true;
        at = i;
        return false;
      });
      check(`options of ${id}`, labels.length > 0 && labels.every(Boolean) && missing.length === 0, JSON.stringify({ labels, missing }));
    }
    if (r.submit_error) same('rejected submit toast', rsSubmitError, r.submit_error);
    same('provider requests', rsRequests, norm(r.requests));
    same('server row', rsRow, refRow);
    same('server settings', rsSettings, refSettings);
    if (r.after_errors && Object.keys(r.after_errors).length) console.log('     upstream submit errors', JSON.stringify(r.after_errors));
    if (r.toasts?.length) console.log('     upstream submit toasts', JSON.stringify(r.toasts));
    console.log('     rs validate requests', JSON.stringify(validateRequests));
    console.log('     rs use-token requests', JSON.stringify(useRequests));
    fs.writeFileSync(`${out}cloud-${provider}-options.json`, JSON.stringify(rsOptions, null, 1));

    // The reference's provider step, as rendered by its component.
    await injectRef(c.route, r.form_html, `cloud-${provider}-form`);
    await injectRef(c.route, r.empty_html, `cloud-${provider}-invalid`);
    await injectRef(c.route, r.filled_html, `cloud-${provider}-filled`);
    await injectRef(c.route, r.error_html, `cloud-${provider}-error`);
  }
} finally {
  for (const u of madeServers) deleteServer(u);
  for (const u of madeTokens) sql(`delete from cloud_provider_tokens where uuid='${u}'`);
  sql("delete from cloud_init_scripts where name='diff-script'");
  await browser.close();
  fake.close();
}
const failed = results.filter((r) => !r).length;
console.log(`\n${results.length - failed}/${results.length} passed`);
process.exit(failed ? 1 : 0);
