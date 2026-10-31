// Terminal screens and refusals, the same steps on both sides, in dark mode.
//   node terminal_states.mjs [--rs=18086]   (resources from compat/diff/terminal_fixture.py up)
// Reference sessions need its terminal port on 127.0.0.1:6002.
// Writes compat/ui/shots/tstate-<name>-{ref,rs}.png; prints what each side shows and PASS/FAIL per state.
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const args = process.argv.slice(2);
const opt = (k, d) => (args.find((a) => a.startsWith(`--${k}=`)) ?? `=${d}`).split('=')[1];
const rsPort = opt('rs', 18086);
const fx = JSON.parse(fs.readFileSync(new URL('./shots/terminal-fixture.json', import.meta.url)));
const secret = fs.readFileSync(new URL('../reference/secrets/ui-login.txt', import.meta.url), 'utf8').match(/^password:\s*(.+)$/m)[1].trim();
const shots = new URL('./shots/', import.meta.url).pathname;
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
