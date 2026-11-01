// Every URL that could reach an "unported" fallback, opened on both sides: compares where it ends up,
// and fails on any page that still says "Not available in coolify-rs yet".
//   node fallback_urls.mjs <state.json> [--rs=18088]
// The state comes from screens2_fixtures.py. Exit status 1 when anything differs.
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const args = process.argv.slice(2);
const state = JSON.parse(fs.readFileSync(args[0], 'utf8'));
const rsPort = (args.find((a) => a.startsWith('--rs=')) ?? '--rs=18088').split('=')[1];
const secret = fs.readFileSync(new URL('../reference/secrets/ui-login.txt', import.meta.url), 'utf8').match(/^password:\s*(.+)$/m)[1].trim();
const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });

async function session(base) {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: 'dark' });
  const page = await ctx.newPage();
  await page.goto(base + '/login');
  await page.fill('input[name=email]', 'alice@example.test');
  await page.fill('input[name=password]', secret);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 }).catch(() => {}), page.click('button[type=submit]')]);
  return page;
}

const root = `/project/${state.project}/environment/${state.env}`;
const server = 'hml20fsj5eles0xfiqidq8j5';
const urls = [
  // Unknown or conditional sections per resource kind.
  ...Object.entries(state.apps).flatMap(([k, u]) =>
    ['nosuch', 'general', 'source', 'swarm', 'preview-deployments', 'healthcheck', 'rollback', 'advanced', 'domains'].map((s) => [`app ${k} ${s}`, `${root}/application/${u}/${s}`]),
  ),
  ...['nosuch', 'general', 'advanced', 'swarm'].map((s) => [`database ${s}`, `${root}/database/${state.databases[0]}/${s}`]),
  ...['nosuch', 'nosuch/advanced', 'nosuch/backups/nosuch/danger', 'nosuch/deeper', 'general'].map((s) => [`service ${s}`, `${root}/service/${state.services[0]}/${s}`]),
  ...(state.stack ?? []).flatMap((u) => [
    [`service stack ${u}`, `${root}/service/${state.services[0]}/${u}`],
    [`service stack ${u} advanced`, `${root}/service/${state.services[0]}/${u}/advanced`],
  ]),
  ['server new unknown', '/servers/new/foo'],
  ['server new manual token', '/servers/new/manual/abc'],
  ['server new manual', '/servers/new/manual'],
  ['server new foo token', '/servers/new/foo/abc'],
  ['server new hetzner token', '/servers/new/hetzner/abc'],
  ['server nosuch', `/server/${server}/nosuch`],
  ['server security', `/server/${server}/security`],
  ['server proxy nosuch', `/server/${server}/proxy/nosuch`],
  ['server sentinel nosuch', `/server/${server}/sentinel/nosuch`],
  ['server canvas', `/server/${server}/canvas`],
];

const sides = [
  ['ref', await session('http://127.0.0.1:18000'), 'http://127.0.0.1:18000'],
  ['rs', await session(`http://127.0.0.1:${rsPort}`), `http://127.0.0.1:${rsPort}`],
];
let same = 0;
let bad = 0;
for (const [name, url] of urls) {
  const out = {};
  for (const [label, page, base] of sides) {
    await page.goto(base + url).catch(() => {});
    await page.waitForTimeout(1500);
    const text = await page.locator('main').first().innerText().catch(() => '');
    out[label] = { path: new URL(page.url()).pathname, unported: /not available in coolify-rs|has not been ported/i.test(text), h: text.length };
  }
  if (out.ref.path === out.rs.path && !out.rs.unported) {
    same++;
  } else {
    bad++;
    console.log('DIFF', name, url, JSON.stringify(out));
  }
}
console.log(`${same}/${urls.length} same, ${bad} differ`);
await browser.close();
process.exit(bad ? 1 : 0);
