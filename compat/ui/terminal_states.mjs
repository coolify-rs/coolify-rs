// Terminal screens and refusals, the same steps on both sides, in dark mode.
//   node terminal_states.mjs [--rs=18086]   (resources from compat/diff/terminal_fixture.py up)
// Reference sessions need its terminal port on 127.0.0.1:6002.
// Writes compat/ui/shots/tstate-<name>-{ref,rs}.png; prints what each side shows and PASS/FAIL per state.
import { chromium } from 'playwright-core';
import fs from 'node:fs';
import { execFileSync } from 'node:child_process';

const args = process.argv.slice(2);
const opt = (k, d) => (args.find((a) => a.startsWith(`--${k}=`)) ?? `=${d}`).split('=')[1];
const rsPort = opt('rs', 18086);
const fx = JSON.parse(fs.readFileSync(new URL('./shots/terminal-fixture.json', import.meta.url)));
const secret = fs.readFileSync(new URL('../reference/secrets/ui-login.txt', import.meta.url), 'utf8').match(/^password:\s*(.+)$/m)[1].trim();
const shots = new URL('./shots/', import.meta.url).pathname;
const tokens = JSON.parse(fs.readFileSync(new URL('../reference/secrets/tokens.json', import.meta.url)));
const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });

async function login(base, user) {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: 'dark' });
  const page = await ctx.newPage();
  await page.addInitScript(() => { try { localStorage.setItem('theme', 'dark'); } catch {} });
  await page.goto(base + '/login');
  await page.fill('input[name=email]', `${user}@example.test`);
  await page.fill('input[name=password]', secret);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 }).catch(() => {}), page.click('button[type=submit]')]);
  for (let i = 0; i < 2; i++) {
    await page.locator('button:has-text("Accept And Close"), button:has-text("Maybe next time")').first().click({ timeout: 800 }).catch(() => {});
  }
  return page;
}

const sides = { ref: 'http://127.0.0.1:18000', rs: `http://127.0.0.1:${rsPort}` };
const pages = { ref: await login(sides.ref, 'alice'), rs: await login(sides.rs, 'alice') };
const res = `/project/${fx.project}/environment/${fx.environment}`;
let pass = 0;
let fail = 0;

async function both(name, path, read, wait = 3000) {
  const seen = {};
  for (const [side, page] of Object.entries(pages)) {
    await page.goto(sides[side] + path);
    await page.waitForTimeout(wait);
    seen[side] = JSON.stringify(await read(page));
    await page.screenshot({ path: `${shots}tstate-${name}-${side}.png` });
  }
  const same = seen.ref === seen.rs;
  same ? pass++ : fail++;
  console.log(`${same ? 'PASS' : 'FAIL'} ${name}\n  ref ${seen.ref}\n  rs  ${seen.rs}`);
}

const label = (p) => p.locator('.terminal-loading-label:visible').allInnerTexts();

// The launcher, a moment after the containers are known.
await both('picker', `${res}/service/${fx.service}/terminal`, async (p) => ({
  label: await label(p),
  options: await p.locator('[data-terminal-target-picker=launcher] button').allInnerTexts(),
}));

// A chosen container: the toolbar picker and its list.
await both('picker-open', `${res}/service/${fx.service}/terminal`, async (p) => {
  await p.locator('[data-terminal-target-picker=launcher] button', { hasText: 'web-' }).click();
  await p.locator('.terminal-session-target-trigger').waitFor();
  await p.waitForTimeout(2500);
  await p.locator('button.terminal-session-target-trigger').click();
  await p.waitForTimeout(300);
  return { mode: await p.getByRole('button', { name: 'Choose terminal mode' }).isVisible(), list: await p.locator('.terminal-target-list button').allInnerTexts() };
});

await both('theme-menu', `${res}/application/${fx.application}/terminal`, async (p) => {
  await p.getByRole('button', { name: 'Choose terminal theme' }).click();
  await p.waitForTimeout(300);
  return p.locator('.console-theme-selector button:visible').allInnerTexts();
});

await both('theme-applied', `${res}/application/${fx.application}/terminal`, async (p) => {
  await p.getByRole('button', { name: 'Choose terminal theme' }).click();
  await p.locator('.console-theme-selector button', { hasText: 'Golden Hour' }).click();
  await p.waitForTimeout(500);
  const out = await p.evaluate(() => ({ theme: document.querySelector('.application-console-shell').dataset.consoleTheme, saved: localStorage.getItem('coolify-console-theme') }));
  await p.evaluate(() => localStorage.setItem('coolify-console-theme', 'system'));
  return out;
});

// A server that does not exist in the team.
await both('other-team-resource', `/server/does-not-exist/terminal`, async (p) => ({ text: await p.locator('body').innerText().then((t) => /404|not found/i.test(t)) }));

// A server that is not functional (a read of an existing row) and a resource with nothing running.
await both('server-unavailable', `/server/${opt('unusable', 'forphxqhvwwipnx1enmed1t7')}/terminal`, async (p) => {
  await p.waitForTimeout(500);
  return { terminal: await p.locator('#terminal').count(), text: await p.getByText('Terminal unavailable').isVisible(), detail: await p.getByText('This server is not functional or terminal access is disabled.').isVisible() };
});
const api = async (path) => fetch(`${sides.rs}/api/v1/databases/${fx.database}/${path}`, { method: 'POST', headers: { Authorization: `Bearer ${tokens.acme_owner_root}` } });
await api('stop');
for (let i = 0; i < 30 && !execFileSync('orb', ['-m', 'coolify-rs-e2e', '-u', 'root', 'docker', 'ps', '-a', '--filter', `name=${fx.database}`, '--format', '{{.State}}']).toString().includes('exited') && execFileSync('orb', ['-m', 'coolify-rs-e2e', '-u', 'root', 'docker', 'ps', '--filter', `name=${fx.database}`, '-q']).toString().trim(); i++) {
  await new Promise((r) => setTimeout(r, 2000));
}
await new Promise((r) => setTimeout(r, 3000));
await both('no-containers', `${res}/database/${fx.database}/terminal`, async (p) => ({
  terminal: await p.locator('#terminal').count(),
  text: await p.getByText('Terminal unavailable').isVisible(),
  detail: await p.getByText('No containers are running, or terminal access is disabled on the destination server.').isVisible(),
}));
await api('start');
await new Promise((r) => setTimeout(r, 8000));

// The messages of the socket, without a session and with bad tokens.
const socketOf = (side) => (side === 'ref' ? 'ws://127.0.0.1:6002/terminal/ws' : `ws://127.0.0.1:${rsPort}/terminal/ws`);
const talk = (page, url, messages) => page.evaluate(async ({ url, messages }) => new Promise((resolve) => {
  const got = [];
  const ws = new WebSocket(url);
  let i = 0;
  const next = () => {
    if (i < messages.length) ws.send(JSON.stringify(messages[i++]));
    else setTimeout(() => { ws.close(1000); }, 500);
  };
  ws.onopen = () => setTimeout(next, 300);
  ws.onmessage = async (e) => { got.push(e.data instanceof Blob ? `binary:${await e.data.text()}` : e.data); setTimeout(next, 100); };
  ws.onclose = (e) => { got.push(`close ${e.code} ${e.reason}`); resolve(got); };
  setTimeout(() => resolve(got.concat('timeout')), 8000);
}), { url, messages });
const compare = async (name, read) => {
  const ref = JSON.stringify(await read('ref'));
  const rs = JSON.stringify(await read('rs'));
  ref === rs ? pass++ : fail++;
  console.log(`${ref === rs ? 'PASS' : 'FAIL'} ${name}\n  ref ${ref}\n  rs  ${rs}`);
};
await compare('socket: ping and checkActive', (side) => talk(pages[side], socketOf(side), [{ ping: true }, { checkActive: 'check' }, { message: 'id\n' }, { command: 'id' }]));
await compare('socket: bad token', (side) => talk(pages[side], socketOf(side), [{ terminalToken: 'docker exec -it x sh' }]));
await compare('socket: unknown token', (side) => talk(pages[side], socketOf(side), [{ terminalToken: 'a'.repeat(64) }]));
const anonymous = {};
for (const side of Object.keys(sides)) {
  const ctx = await browser.newContext();
  anonymous[side] = await ctx.newPage();
  await anonymous[side].goto(sides[side] + '/login');
  await anonymous[side].context().clearCookies();
}
await compare('socket: no session', (side) => talk(anonymous[side], socketOf(side), []));

// /terminal for a member, and the resource page for a member.
const members = { ref: await login(sides.ref, 'bob'), rs: await login(sides.rs, 'bob') };
for (const [name, path] of [['member-index', '/terminal'], ['member-server', `/server/${fx.server}/terminal`], ['member-application', `${res}/application/${fx.application}/terminal`]]) {
  const seen = {};
  for (const [side, page] of Object.entries(members)) {
    await page.goto(sides[side] + path);
    await page.waitForTimeout(2000);
    const body = await page.locator('body').innerText();
    seen[side] = { forbidden: /403|restricted to team administrators|forbidden/i.test(body), shell: await page.locator('#terminal').count() };
    await page.screenshot({ path: `${shots}tstate-${name}-${side}.png` });
  }
  const same = JSON.stringify(seen.ref) === JSON.stringify(seen.rs);
  same ? pass++ : fail++;
  console.log(`${same ? 'PASS' : 'FAIL'} ${name}\n  ref ${JSON.stringify(seen.ref)}\n  rs  ${JSON.stringify(seen.rs)}`);
}

// Sign in as a member, hit the socket directly: a token cannot be had and the socket says so.
for (const [side, page] of Object.entries(members)) {
  const code = await page.evaluate(async (url) => new Promise((resolve) => {
    const ws = new WebSocket(url);
    ws.onclose = (e) => resolve(e.code);
    setTimeout(() => resolve('open'), 5000);
  }), side === 'ref' ? 'ws://127.0.0.1:6002/terminal/ws' : `ws://127.0.0.1:${rsPort}/terminal/ws`);
  const good = code === 4401;
  good ? pass++ : fail++;
  console.log(`${good ? 'PASS' : 'FAIL'} member socket ${side}: ${code}`);
}
console.log(`${pass}/${pass + fail} passed`);
await browser.close();
