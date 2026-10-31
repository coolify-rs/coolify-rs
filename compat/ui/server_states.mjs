// Screenshots of what plain page shots do not reach: modals, empty and error states, a server that is not
// functional, the member role, Sentinel logs. Both UIs read the same rows (shared database).
//   node server_states.mjs --rs=18087 --a=<reference server uuid> --b=<coolify-rs server uuid>
// Output: shots/<name>-{ref,rs}.png, then python3 pair.py <names printed at the end>.
// Creates one server without a route (198.51.100.77) for the non-functional state and removes it at the end.
import { chromium } from 'playwright-core';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';

const opt = (k) => (process.argv.find((a) => a.startsWith(`--${k}=`)) ?? '=').split('=')[1];
const rsPort = opt('rs') || 18087;
const A = opt('a');
const B = opt('b');
const secrets = new URL('../reference/secrets/', import.meta.url);
const password = fs.readFileSync(new URL('ui-login.txt', secrets), 'utf8').match(/^password:\s*(.+)$/m)[1].trim();
const TOKEN = JSON.parse(fs.readFileSync(new URL('tokens.json', secrets), 'utf8')).acme_owner_root;
const out = new URL('./shots/', import.meta.url).pathname;
const REF = 'http://127.0.0.1:18000';
const RS = `http://127.0.0.1:${rsPort}`;
const KEY_UUID = 'hv41morfjv9x0av5kakzdmv8';
const sql = (q) => execFileSync('docker', ['exec', 'coolify-rs-reference-postgres-1', 'psql', '-U', 'coolify', '-d', 'coolify', '-At', '-c', q]).toString().trim();
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
async function login(base, user) {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: 'dark' });
  const page = await ctx.newPage();
  await page.addInitScript(() => {
    try {
      localStorage.setItem('theme', 'dark');
    } catch {}
  });
  await page.goto(`${base}/login`);
  await page.fill('input[name=email]', `${user}@example.test`);
  await page.fill('input[name=password]', password);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 }).catch(() => {}), page.click('button[type=submit]')]);
  return page;
}
async function hidePopups(page) {
  await page.evaluate(() => {
    for (const d of document.querySelectorAll('div')) {
      if (getComputedStyle(d).position === 'fixed' && /Cannot connect to real-time service|Love Coolify|No notifications enabled/.test(d.innerText ?? '')) d.remove();
    }
  }).catch(() => {});
}
async function clickVisible(page, text) {
  const re = new RegExp(`^\\s*${text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\s*$`, 'i');
  const all = await page.locator('button, a').filter({ hasText: re }).all();
  for (const el of all.reverse()) {
    if (await el.isVisible().catch(() => false)) {
      await el.click({ timeout: 3000 });
      return true;
    }
  }
  return false;
}
const shots = [];
const ref = { alice: await login(REF, 'alice'), bob: await login(REF, 'bob') };
const rs = { alice: await login(RS, 'alice'), bob: await login(RS, 'bob') };
async function shot(name, refPath, rsPath, { user = 'alice', clicks = [], wait = 2500 } = {}) {
  for (const [label, sides, base, path] of [['ref', ref, REF, refPath], ['rs', rs, RS, rsPath]]) {
    const page = sides[user];
    await page.goto(base + path);
    await sleep(wait);
    await hidePopups(page);
    const missing = [];
    for (const c of clicks) {
      if (!(await clickVisible(page, c))) missing.push(c);
      await sleep(600);
    }
    await page.screenshot({ path: `${out}${name}-${label}.png` });
    if (missing.length) console.log(`${name} ${label}: no button ${missing.join(', ')}`);
  }
  shots.push(name);
}

const r = await fetch(`${REF}/api/v1/servers`, {
  method: 'POST',
  headers: { Authorization: `Bearer ${TOKEN}`, Accept: 'application/json', 'Content-Type': 'application/json' },
  body: JSON.stringify({ name: 'diff-states', ip: '198.51.100.77', private_key_uuid: KEY_UUID }),
});
const dead = (await r.json()).uuid;
try {
  const both = (p) => [`/server/${A}${p}`, `/server/${B}${p}`];
  // Modals.
  await shot('m-proxy-stop', ...both('/proxy'), { clicks: ['Switch proxy'] });
  await shot('m-proxy-reset', ...both('/proxy'), { clicks: ['Reset configuration'] });
  await shot('m-dest-add', ...both('/destinations'), { clicks: ['+ Add'] });
  await shot('m-key-add', ...both('/private-key'), { clicks: ['Add', 'Add manually'] });
  await shot('m-ca-regen', ...both('/ca-certificate'), { clicks: ['Regenerate'] });
  await shot('m-ca-show', ...both('/ca-certificate'), { clicks: ['Show certificate'] });
  await shot('m-cleanup', ...both('/docker-cleanup'), { clicks: ['Run cleanup'] });
  await shot('m-sentinel-restore', ...both('/sentinel'), { clicks: ['Restore defaults'] });
  await shot('m-registry-new', ...both('/registries'), { clicks: ['New'] });
  await shot('m-registries-new', '/registries', '/registries', { clicks: ['New'] });
  await shot('m-analytics', ...both('/analytics'), { clicks: ['Enable traffic analytics'] });
  await shot('m-dynamic-new', ...both('/proxy/dynamic'), { clicks: ['+ Add'] });
  // Sentinel logs and empty lists.
  await shot('s-sentinel-logs', ...both('/sentinel/logs'), { wait: 4000 });
  await shot('s-resources', ...both('/resources'));
  await shot('s-resources-unmanaged', ...both('/resources'), { clicks: ['Unmanaged'] });
  await shot('s-images', ...both('/images'), { wait: 4000 });
  await shot('s-tls', ...both('/proxy/certificates'), { wait: 4000 });
  await shot('s-dynamic', ...both('/proxy/dynamic'), { wait: 4000 });
  await shot('s-log-drains', ...both('/log-drains'));
  await shot('s-cleanup', ...both('/docker-cleanup'));
  // A server that is not functional.
  for (const p of ['', '/proxy', '/destinations', '/resources', '/images', '/sentinel', '/sentinel/logs', '/proxy/logs', '/proxy/certificates', '/docker-cleanup', '/metrics', '/registries', '/log-drains', '/analytics']) {
    await shot(`d${p.replace(/\W+/g, '-') || '-general'}`, `/server/${dead}${p}`, `/server/${dead}${p}`, { wait: p ? 2500 : 2000 });
  }
  // A member.
  for (const p of ['', '/proxy', '/destinations', '/private-key', '/ca-certificate', '/sentinel', '/docker-cleanup', '/log-drains', '/images', '/resources', '/advanced']) {
    await shot(`b${p.replace(/\W+/g, '-') || '-general'}`, ...both(p), { user: 'bob' });
  }
  await shot('b-registries', '/registries', '/registries', { user: 'bob' });
  await shot('b-destination-list', '/destinations', '/destinations', { user: 'bob' });
} finally {
  const sid = sql(`select id from servers where uuid='${dead}'`);
  for (const q of [`delete from standalone_dockers where server_id=${sid}`, `delete from swarm_dockers where server_id=${sid}`, `delete from ssl_certificates where server_id=${sid}`, `delete from server_settings where server_id=${sid}`, `delete from servers where id=${sid}`]) sql(q);
  await browser.close();
}
console.log(shots.join(' '));
