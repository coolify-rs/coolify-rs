// Side-by-side screenshots of guest pages, without signing in.
//   node guest_shots.mjs [--rs=18086] [--light] [--host=127.0.0.1] name=/path[@rsPath] ...
// Output as shots.mjs: compat/ui/shots/<name>-{ref,rs}.png, then python3 pair.py <name>.
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const args = process.argv.slice(2);
const opt = (k, d) => (args.find((a) => a.startsWith(`--${k}=`)) ?? `=${d}`).split('=')[1];
const flag = (k) => args.includes(`--${k}`);
const rsPort = opt('rs', 18086);
const host = opt('host', '127.0.0.1');
const targets = args.filter((a) => !a.startsWith('--')).map((a) => {
  const name = a.slice(0, a.indexOf('='));
  const [refPath, rsPath] = a.slice(a.indexOf('=') + 1).split('@');
  return { name, refPath, rsPath: rsPath ?? refPath };
});
const out = new URL('./shots/', import.meta.url).pathname;
fs.mkdirSync(out, { recursive: true });
const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
for (const t of targets) {
  for (const [label, base, path] of [['ref', `http://${host}:18000`, t.refPath], ['rs', `http://${host}:${rsPort}`, t.rsPath]]) {
    const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: flag('light') ? 'light' : 'dark' });
    const page = await ctx.newPage();
    await page.addInitScript((light) => { try { localStorage.setItem('theme', light ? 'light' : 'dark'); } catch {} }, flag('light'));
    await page.route('**/*', (r) => {
      const u = new URL(r.request().url());
      return u.hostname === host ? r.continue() : r.abort();
    });
    await page.goto(base + path);
    await page.waitForTimeout(1500);
    const file = `${out}${t.name}-${label}.png`;
    await page.screenshot({ path: file });
    console.log(file, page.url());
    await ctx.close();
  }
}
await browser.close();
