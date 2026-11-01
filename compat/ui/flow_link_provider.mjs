// The Link provider menu and modals of the server page, both sides.
//   COOLIFY_RS_CLOUD_API_BASE=http://127.0.0.1:18189 scripts/dev-server.sh 18088   (must be running)
//   node flow_link_provider.mjs --rs=18088 [--light]
// The reference talks to the real provider hosts once a search passes its checks, so on the reference this only opens the
// menu and modals and clicks searches that stop before any request (no token; token without an id). The searches that
// reach a provider and the link run only on coolify-rs, against the local fake here.
// Creates one server and four tokens, tracked by id and deleted in the finally block.
import { chromium } from 'playwright-core';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';

const args = process.argv.slice(2);
const opt = (k, d) => (args.find((a) => a.startsWith(`--${k}=`)) ?? `=${d}`).split('=')[1];
const light = args.includes('--light');
const prefix = light ? 'lpl' : 'lp';
const rs = `http://127.0.0.1:${opt('rs', 18088)}`;
const ref = 'http://127.0.0.1:18000';
const here = new URL('./', import.meta.url).pathname;
const secret = fs.readFileSync(`${here}../reference/secrets/ui-login.txt`, 'utf8').match(/^password:\s*(.+)$/m)[1].trim();
const apiToken = JSON.parse(fs.readFileSync(`${here}../reference/secrets/tokens.json`, 'utf8')).acme_owner_root;
const psql = (q) => execFileSync('docker', ['exec', 'coolify-rs-reference-postgres-1', 'psql', '-U', 'coolify', '-d', 'coolify', '-Atc', q], { encoding: 'utf8' }).trim();
const encrypted = execFileSync('docker', ['exec', 'coolify-rs-reference-coolify-1', 'php', '/var/www/html/artisan', 'tinker', '--execute', "echo Illuminate\\Support\\Facades\\Crypt::encryptString('local-fake-token');"], { encoding: 'utf8' }).trim();

const IP = '10.254.77.9';
const FAKE = {
  'GET /hetzner/v1/servers': { servers: [{ id: 4242, name: 'fake-hetzner', status: 'running', public_net: { ipv4: { ip: IP }, ipv6: { ip: '2a01:4f8::/64' } } }], meta: { pagination: { next_page: null } } },
  'GET /hetzner/v1/servers/4242': { server: { id: 4242, name: 'fake-hetzner', status: 'running' } },
  'GET /digitalocean/v2/droplets': { droplets: [{ id: 77, name: 'fake-droplet', status: 'active', networks: { v4: [{ type: 'public', ip_address: IP }], v6: [] } }], links: {} },
  'GET /digitalocean/v2/droplets/77': { droplet: { id: 77, name: 'fake-droplet', status: 'active', networks: { v4: [{ type: 'public', ip_address: '10.254.77.10' }], v6: [] } } },
  'GET /vultr/v2/instances': { instances: [{ id: 'vu-1', label: 'fake-vultr', status: 'active', main_ip: IP }], meta: { links: { next: '' } } },
  'GET /vultr/v2/instances/vu-1': { instance: { id: 'vu-1', label: 'fake-vultr', status: 'active', main_ip: IP } },
  'GET /hostinger/api/vps/v1/virtual-machines': [{ id: 17923, hostname: 'fake-vps', state: 'running', ipv4: [{ address: IP }], ipv6: [] }],
  'GET /hostinger/api/vps/v1/virtual-machines/17923': { id: 17923, hostname: 'fake-vps', state: 'running', ipv4: [{ address: '10.254.77.11' }], ipv6: [] },
};
const fake = http.createServer((req, res) => {
  const key = `${req.method} ${req.url.split('?')[0]}`;
  const body = FAKE[key];
  res.writeHead(body ? 200 : 404, { 'Content-Type': 'application/json' });
  res.end(JSON.stringify(body ?? { error: { message: 'not found at the fake' }, message: 'not found at the fake' }));
});
await new Promise((r) => fake.listen(18189, '127.0.0.1', r));

const made = { server: null, tokens: [] };
const results = [];
const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
async function open(base, path) {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: light ? 'light' : 'dark' });
  await ctx.route('**/*', (r) => (new URL(r.request().url()).hostname === '127.0.0.1' ? r.continue() : r.abort()));
  await ctx.addInitScript((l) => {
    try {
      localStorage.setItem('theme', l ? 'light' : 'dark');
      localStorage.setItem('popupRealtime', 'disabled');
      localStorage.setItem('popupNotification', String(Date.now()));
      localStorage.setItem('popupSponsorship', String(Date.now()));
    } catch {}
  }, light);
  const page = await ctx.newPage();
  await page.goto(`${base}/login`);
  await page.fill('input[name=email]', 'alice@example.test');
  await page.fill('input[name=password]', secret);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 20000 }).catch(() => {}), page.click('button[type=submit]')]);
  await page.goto(base + path);
  await page.waitForLoadState('networkidle', { timeout: 6000 }).catch(() => {});
  await page.waitForTimeout(1200);
  return page;
}
const shot = (page, name, side) => page.screenshot({ path: `${here}shots/${prefix}-${name}-${side}.png` });
const check = (name, ok, detail = '') => { results.push(ok); console.log(`${ok ? 'ok  ' : 'FAIL'} ${name} ${ok ? '' : detail}`); };
const vis = (page, sel) => page.locator(sel).filter({ visible: true });
const menuButton = (page) => vis(page, 'button:has-text("Link provider")').first();
const item = (page, label) => vis(page, `button:text-is("${label}")`).first();
const scope = (page) => page.locator('.application-settings-form:has-text("for provider status and power controls")').filter({ visible: true }).first();
const selectToken = async (page, name) => { await scope(page).locator('button:has-text("Select a token")').first().click(); await page.waitForTimeout(300); await page.locator(`[role=option]:has-text("${name}")`).filter({ visible: true }).first().click(); await page.waitForTimeout(700); };

try {
  const key = psql("select k.uuid from private_keys k join servers s on s.private_key_id=k.id where s.uuid='hml20fsj5eles0xfiqidq8j5'");
  const team = psql("select team_id from servers where uuid='hml20fsj5eles0xfiqidq8j5'");
  const created = await fetch(`${rs}/api/v1/servers`, { method: 'POST', headers: { Authorization: `Bearer ${apiToken}`, 'Content-Type': 'application/json' }, body: JSON.stringify({ name: 'lp-link-fixture', ip: IP, port: 22, user: 'root', private_key_uuid: key }) });
  const uuid = (await created.json()).uuid;
  if (!uuid) throw new Error(`server fixture not created: ${created.status}`);
  made.server = psql(`select id from servers where uuid='${uuid}'`);
  for (const provider of ['hetzner', 'digitalocean', 'vultr', 'hostinger']) {
    made.tokens.push(psql(`insert into cloud_provider_tokens (team_id, provider, token, name, uuid, created_at, updated_at) values (${team}, '${provider}', '${encrypted}', 'lp-${provider}', 'lp${provider}${Date.now()}', now(), now()) returning id`).split('\n')[0]);
  }
  const path = `/server/${uuid}`;
  const pages = { ref: await open(ref, path), rs: await open(rs, path) };
  for (const [side, page] of Object.entries(pages)) {
    await shot(page, 'page', side);
    await menuButton(page).click(); await page.waitForTimeout(400);
    await shot(page, 'menu', side);
    for (const label of ['Hetzner', 'DigitalOcean', 'Vultr', 'Hostinger']) {
      await item(page, label).click(); await page.waitForTimeout(600);
      await shot(page, `modal-${label}`, side);
      await page.keyboard.press('Escape'); await page.waitForTimeout(400);
      if (!(await item(page, 'Vultr').isVisible().catch(() => false))) { await menuButton(page).click(); await page.waitForTimeout(300); }
    }
    await item(page, 'Hetzner').click(); await page.waitForTimeout(500);
    // No token: returns before any request on both sides.
    await scope(page).locator('button:has-text("Search ID")').click(); await page.waitForTimeout(900);
    await shot(page, 'no-token-id', side);
    await scope(page).locator('button:has-text("Search by server IP")').click(); await page.waitForTimeout(900);
    await shot(page, 'no-token-ip', side);
    // A token and no id: also returns before any request.
    await selectToken(page, 'lp-hetzner');
    await scope(page).locator('button:has-text("Search ID")').click(); await page.waitForTimeout(900);
    await shot(page, 'token-no-id', side);
  }
  await pages.ref.context().close();

  // From here on coolify-rs only, against the fake.
  const page = pages.rs;
  const text = async () => (await scope(page).innerText()).replace(/\s+/g, ' ');
  const manual = scope(page).locator('input').first();
  await manual.fill('999'); await scope(page).locator('button:has-text("Search ID")').click(); await page.waitForTimeout(900);
  await shot(page, 'failed', 'rs');
  check('search of an unknown id shows the provider error', (await text()).includes('Failed to fetch Hetzner server:'), await text());
  await manual.fill('4242'); await scope(page).locator('button:has-text("Search ID")').click(); await page.waitForTimeout(900);
  await shot(page, 'matched', 'rs');
  check('search by id finds the server', /fake-hetzner.*4242.*Running.*Match found/.test(await text()), await text());
  await scope(page).locator('button:has-text("Search by server IP")').click(); await page.waitForTimeout(900);
  check('search by ip finds the server', /fake-hetzner.*4242/.test(await text()), await text());
  await scope(page).locator('button:has-text("Link resource")').click(); await page.waitForTimeout(1000);
  await shot(page, 'linked', 'rs');
  const row = () => psql(`select cloud_provider_token_id, hetzner_server_id, hetzner_server_status, digitalocean_droplet_id, digitalocean_droplet_status, vultr_instance_id, vultr_instance_status, hostinger_virtual_machine_id, hostinger_virtual_machine_status, ip from servers where id=${made.server}`);
  check('hetzner row', row() === `${made.tokens[0]}|4242|running|||||||${IP}`, row());
  await menuButton(page).click(); await page.waitForTimeout(300);
  check('hetzner leaves the menu', !(await item(page, 'Hetzner').isVisible().catch(() => false)));
  const linkOther = async (label, token, id, expected) => {
    if (!(await item(page, label).isVisible().catch(() => false))) { await menuButton(page).click(); await page.waitForTimeout(300); }
    await item(page, label).click(); await page.waitForTimeout(500);
    await selectToken(page, token);
    await scope(page).locator('input').first().fill(id);
    await scope(page).locator('button:has-text("Search ID")').click(); await page.waitForTimeout(900);
    await scope(page).locator('button:has-text("Link resource")').click(); await page.waitForTimeout(1000);
    check(`${label} row`, row() === expected, row());
  };
  await linkOther('DigitalOcean', 'lp-digitalocean', '77', `${made.tokens[1]}|4242|running|77|active|||||10.254.77.10`);
  await linkOther('Vultr', 'lp-vultr', 'vu-1', `${made.tokens[2]}|4242|running|77|active|vu-1|active|||10.254.77.10`);
  await linkOther('Hostinger', 'lp-hostinger', '17923', `${made.tokens[3]}|4242|running|77|active|vu-1|active|17923|running|10.254.77.11`);
  check('menu gone once all four are linked', !(await menuButton(page).isVisible().catch(() => false)));
} catch (e) {
  results.push(false);
  console.log('FAIL', String(e.message).split('\n')[0].slice(0, 300));
} finally {
  await browser.close();
  fake.close();
  if (made.server) {
    psql(`delete from standalone_dockers where server_id=${made.server}`);
    psql(`delete from server_settings where server_id=${made.server}`);
    psql(`delete from servers where id=${made.server}`);
  }
  for (const t of made.tokens) psql(`delete from cloud_provider_tokens where id=${t}`);
  console.log(`${results.filter(Boolean).length}/${results.length}; left: server ${psql(`select count(*) from servers where name='lp-link-fixture'`)}, tokens ${psql("select count(*) from cloud_provider_tokens where name like 'lp-%'")}`);
}
