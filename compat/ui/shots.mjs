// Side-by-side screenshots of the reference Coolify (:18000) and coolify-rs.
//   node shots.mjs [--rs=18080] [--w=1440] [--h=900] [--light] [--full] [--user=alice] name=/path[@rsPath] ...
//   Output: compat/ui/shots/<name>-{ref,rs,pair}.png (pair needs Pillow, see pair.py)
// The test password is read from the fixture file and never printed.
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const args = process.argv.slice(2);
const opt = (k, d) => (args.find((a) => a.startsWith(`--${k}=`)) ?? `=${d}`).split('=')[1];
const flag = (k) => args.includes(`--${k}`);
const width = Number(opt('w', 1440));
const height = Number(opt('h', 900));
const user = opt('user', 'alice');
const rsPort = opt('rs', 18080);
const targets = args.filter((a) => !a.startsWith('--')).map((a) => {
  const name = a.slice(0, a.indexOf('='));
  const [refPath, rsPath] = a.slice(a.indexOf('=') + 1).split('@');
  return { name, refPath, rsPath: rsPath ?? refPath };
});
const secret = fs.readFileSync(new URL('../reference/secrets/ui-login.txt', import.meta.url), 'utf8').match(/^password:\s*(.+)$/m)[1].trim();
const out = new URL('./shots/', import.meta.url).pathname;
fs.mkdirSync(out, { recursive: true });
const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });

export async function session(base) {
  const ctx = await browser.newContext({ viewport: { width, height }, colorScheme: flag('light') ? 'light' : 'dark' });
  const page = await ctx.newPage();
  await page.addInitScript((light) => { try { localStorage.setItem('theme', light ? 'light' : 'dark'); } catch {} }, flag('light'));
  await page.goto(base + '/login');
  await page.fill('input[name=email]', `${user}@example.test`);
  await page.fill('input[name=password]', secret);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 }).catch(() => {}), page.click('button[type=submit]')]);
  return page;
}

const sides = [['ref', await session('http://127.0.0.1:18000'), 'http://127.0.0.1:18000'], ['rs', await session(`http://127.0.0.1:${rsPort}`), `http://127.0.0.1:${rsPort}`]];
for (const t of targets) {
  for (const [label, page, base] of sides) {
    await page.goto(base + (label === 'ref' ? t.refPath : t.rsPath));
    await page.waitForTimeout(2500);
    // Upstream's first-run notification popup is not part of the page.
    await page.locator('button:has-text("Accept And Close"), button:has-text("Accept and Close")').first().click({ timeout: 500 }).catch(() => {});
    const file = `${out}${t.name}-${label}.png`;
    await page.screenshot({ path: file, fullPage: flag('full') });
    console.log(file);
  }
}
await browser.close();
