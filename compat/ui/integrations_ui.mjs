// The secret manager section and the DNS menu on both sides, in dark mode. The tokens are rows only: the
// reference is never asked to browse keys, so it contacts nothing.
//   node integrations_ui.mjs --rs=18091 --fixture=<json file from integrations_ui_fixture.py>
// then `python3 pair.py <name>` for each name it prints. The password is read here and never printed.
import { readFileSync } from 'node:fs';
import { chromium } from 'playwright-core';

const arg = (k) => process.argv.find((a) => a.startsWith(`--${k}=`))?.split('=')[1];
const rsPort = arg('rs') ?? '18091';
const fixture = JSON.parse(readFileSync(arg('fixture'), 'utf8'));
const password = /^password:\s*(.+)$/m.exec(readFileSync('../reference/secrets/ui-login.txt', 'utf8'))[1].trim();
const out = 'shots/';
const sides = { ref: 'http://127.0.0.1:18000', rs: `http://127.0.0.1:${rsPort}` };
const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });

async function session(base) {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 1100 }, colorScheme: 'dark' });
  const page = await ctx.newPage();
  await page.goto(`${base}/login`);
  await page.waitForTimeout(800);
  await page.fill('input[type=email], input[name=email]', 'alice@example.test');
  await page.fill('input[type=password]', password);
  await page.locator('button[type=submit]').first().click().catch(() => {});
  await page.waitForTimeout(2500);
  return page;
}

async function dismiss(page) {
  for (const label of ['Acknowledge', 'Dismiss']) {
    const b = page.locator(`button:has-text("${label}")`).first();
    if (await b.isVisible().catch(() => false)) await b.click().catch(() => {});
  }
}

const names = [];
for (const [side, base] of Object.entries(sides)) {
  let page;
  try {
    page = await session(base);
    for (const c of fixture.apps) {
      const env = `/project/${fixture.project}/environment/${fixture.environment}/application/${c.uuid}`;
      await page.goto(`${base}${env}/environment-variables`);
      await page.waitForTimeout(2500);
      await dismiss(page);
      const section = page.locator('h3:has-text("Secret manager"), h2:has-text("Secret manager")').first();
      await section.scrollIntoViewIfNeeded().catch(() => {});
      await page.waitForTimeout(400);
      await page.screenshot({ path: `${out}int-secret-${c.name}-${side}.png` });
      if (side === 'ref') names.push(`int-secret-${c.name}`);
    }
    const domains = `/project/${fixture.project}/environment/${fixture.environment}/application/${fixture.apps[0].uuid}/domains`;
    await page.goto(`${base}${domains}`);
    await page.waitForTimeout(2500);
    await dismiss(page);
    await page.locator('button:has-text("DNS")').first().click();
    await page.waitForTimeout(500);
    await page.screenshot({ path: `${out}int-dns-menu-${side}.png` });
    await page.locator('button:has-text("Manual records")').first().click();
    await page.waitForTimeout(1200);
    await page.screenshot({ path: `${out}int-dns-records-${side}.png` });
    if (side === 'ref') names.push('int-dns-menu', 'int-dns-records');
  } catch (e) {
    console.log(side, 'failed:', String(e.message).replaceAll(password, '***').slice(0, 300));
  } finally {
    await page?.context().close();
  }
}
await browser.close();
console.log(names.join(' '));
