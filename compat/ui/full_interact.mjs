// Interaction states on both sides: opens modals and dropdowns, submits forms, catches toasts, and shoots the result.
//   node full_interact.mjs --rs=18088 [--light] [--only=substr] scenarios.json
// Scenario: { name, path, steps: [ {click: selector, nth?} | {fill: selector, value} | {key: 'Meta+k'} | {wait: ms} ], w?, h? }
// Output: shots/ix-<name>-{ref,rs}.png and a step log. A step that fails is reported for that side.
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const args = process.argv.slice(2);
const opt = (k, d) => (args.find((a) => a.startsWith(`--${k}=`)) ?? `=${d}`).split('=')[1];
const flag = (k) => args.includes(`--${k}`);
const light = flag('light');
const only = opt('only', '');
const prefix = light ? 'ixl' : 'ix';
const scenarios = JSON.parse(fs.readFileSync(args.find((a) => !a.startsWith('--')), 'utf8')).filter((s) => s.name.includes(only));
const secret = fs.readFileSync(new URL('../reference/secrets/ui-login.txt', import.meta.url), 'utf8').match(/^password:\s*(.+)$/m)[1].trim();
const out = new URL('./shots/', import.meta.url).pathname;
const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });

async function context(w = 1440, h = 900, storageState) {
  const ctx = await browser.newContext({ viewport: { width: w, height: h }, colorScheme: light ? 'light' : 'dark', storageState });
  await ctx.route('**/*', (r) => (new URL(r.request().url()).hostname === '127.0.0.1' ? r.continue() : r.abort()));
  await ctx.addInitScript((l) => { try { localStorage.setItem('theme', l ? 'light' : 'dark'); } catch {} }, light);
  return ctx;
}
async function login(base) {
  const ctx = await context();
  const page = await ctx.newPage();
  await page.goto(base + '/login');
  await page.fill('input[name=email]', 'alice@example.test');
  await page.fill('input[name=password]', secret);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 20000 }).catch(() => {}), page.click('button[type=submit]')]);
  const state = await ctx.storageState();
  await ctx.close();
  return state;
}
const sides = { ref: { base: 'http://127.0.0.1:18000' }, rs: { base: `http://127.0.0.1:${opt('rs', 18088)}` } };
for (const s of Object.values(sides)) s.state = await login(s.base);

for (const sc of scenarios) {
  const log = {};
  await Promise.all(Object.entries(sides).map(async ([label, s]) => {
    const ctx = await context(sc.w, sc.h, s.state);
    const page = await ctx.newPage();
    const fails = [];
    await page.goto(s.base + sc.path, { timeout: 45000 });
    await page.waitForLoadState('networkidle', { timeout: 6000 }).catch(() => {});
    await page.waitForTimeout(1200);
    for (let i = 0; i < 2; i++) {
      const popup = page.locator('button:has-text("Accept And Close"), button:has-text("Accept and Close"), button:has-text("Maybe next time")').first();
      if (await popup.isVisible().catch(() => false)) await popup.click({ timeout: 2000 }).catch(() => {});
    }
    for (const st of sc.steps) {
      try {
        if (st.click) await page.locator(st.click).filter({ visible: true }).nth(st.nth ?? 0).click({ timeout: 4000 });
        else if (st.fill) await page.locator(st.fill).filter({ visible: true }).nth(st.nth ?? 0).fill(st.value, { timeout: 4000 });
        else if (st.key) await page.keyboard.press(st.key);
        else if (st.wait) await page.waitForTimeout(st.wait);
      } catch (e) {
        fails.push(`${JSON.stringify(st)}: ${String(e.message).split('\n')[0].slice(0, 80)}`);
      }
      await page.waitForTimeout(st.wait ? 0 : 500);
    }
    await page.waitForTimeout(sc.settle ?? 500);
    await page.screenshot({ path: `${out}${prefix}-${sc.name}-${label}.png` });
    log[label] = { fails, toast: await page.locator('.toast, [data-sonner-toast], [role=status]').allInnerTexts().catch(() => []) };
    await ctx.close();
  }));
  console.log(sc.name, log.ref.fails.length || log.rs.fails.length ? JSON.stringify({ ref: log.ref.fails, rs: log.rs.fails }) : 'steps ok', JSON.stringify({ ref: log.ref.toast, rs: log.rs.toast }).slice(0, 200));
}
await browser.close();
