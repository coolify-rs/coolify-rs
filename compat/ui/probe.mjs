// Prints the final URL, HTTP status, title and first text of pages on the reference and coolify-rs.
//   node probe.mjs [--rs=18092] [--user=alice] /path ...
// The test password is read from the fixture file and never printed.
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const args = process.argv.slice(2);
const opt = (k, d) => (args.find((a) => a.startsWith(`--${k}=`)) ?? `=${d}`).split('=')[1];
const paths = args.filter((a) => !a.startsWith('--'));
const secret = fs.readFileSync(new URL('../reference/secrets/ui-login.txt', import.meta.url), 'utf8').match(/^password:\s*(.*)$/m)[1].trim();
const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
const user = opt('user', 'alice');
for (const [label, base] of [['ref', 'http://127.0.0.1:18000'], ['rs', `http://127.0.0.1:${opt('rs', 18092)}`]]) {
  const ctx = await browser.newContext({ colorScheme: 'dark' });
  const page = await ctx.newPage();
  await page.goto(`${base}/login`);
  await page.fill('input[name=email]', `${user}@example.test`);
  await page.fill('input[name=password]', secret);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 }).catch(() => {}), page.keyboard.press('Enter')]);
  for (const p of paths) {
    const r = await page.goto(base + p).catch(() => null);
    await page.waitForTimeout(800);
    const text = (await page.locator('body').innerText().catch(() => '')).replace(/\s+/g, ' ');
    const i = text.indexOf('Tags ');
    console.log(`${label} ${p} -> ${r?.status()} ${new URL(page.url()).pathname} | ${await page.title()} | ${text.slice(i >= 0 ? i + 5 : 0, (i >= 0 ? i + 5 : 0) + 110)}`);
  }
  await ctx.close();
}
await browser.close();
