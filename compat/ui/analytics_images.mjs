// Which flag and favicon images of the analytics page loaded, on each side, and the console errors.
//   node analytics_images.mjs --rs=18087 --path=/analytics
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const argv = process.argv.slice(2);
const opt = (k, d) => (argv.find((a) => a.startsWith(`--${k}=`)) ?? `=${d}`).split('=').slice(1).join('=');
const secret = fs.readFileSync(new URL('../reference/secrets/ui-login.txt', import.meta.url), 'utf8').match(/^password:\s*(.+)$/m)[1].trim();
const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
const result = {};
for (const [side, base] of [['ref', 'http://127.0.0.1:18000'], ['rs', `http://127.0.0.1:${opt('rs', 18087)}`]]) {
  const page = await (await browser.newContext({ viewport: { width: 1440, height: 900 } })).newPage();
  const errors = [];
  page.on('console', (m) => m.type() === 'error' && errors.push(m.text().slice(0, 200)));
  page.on('pageerror', (e) => errors.push(String(e).slice(0, 200)));
  await page.goto(`${base}/login`);
  await page.fill('input[name=email]', 'alice@example.test');
  await page.fill('input[name=password]', secret);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 }).catch(() => {}), page.click('button[type=submit]')]);
  errors.length = 0;
  await page.goto(base + opt('path', '/analytics'));
  await page.waitForFunction(() => document.querySelector('#analytics-referer-section img'), null, { timeout: 30000 }).catch(() => {});
  for (const id of ['analytics-country-section', 'analytics-referer-section']) {
    await page.locator(`#${id}`).scrollIntoViewIfNeeded().catch(() => {});
    await page.waitForTimeout(2500);
  }
  result[side] = {
    images: await page.evaluate(() =>
      [...document.querySelectorAll('section[id^="analytics-"] img')].map((i) => ({ src: i.getAttribute('src'), srcset: i.getAttribute('srcset'), loaded: i.complete && i.naturalWidth > 0, hidden: i.style.visibility === 'hidden' })),
    ),
    errors,
  };
}
await browser.close();
console.log(JSON.stringify(result, null, 1));
