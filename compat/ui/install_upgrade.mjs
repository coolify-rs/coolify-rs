// Upgrades a coolify-rs install from its UI: Settings, check for updates, Upgrade now, then the progress view.
//   node install_upgrade.mjs <tunnel port> <password file> <version expected afterwards>
// The password is read from the file and never printed.
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const [port, passFile, target] = process.argv.slice(2);
const base = `http://127.0.0.1:${port}`;
const password = fs.readFileSync(passFile, 'utf8');
const out = new URL('./shots/', import.meta.url).pathname;
const browser = await chromium.launch({
  executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  headless: true,
});
const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: 'dark' });
await ctx.addInitScript(() => {
  try {
    localStorage.setItem('theme', 'dark');
  } catch {}
});
const page = await ctx.newPage();
const fail = async (message) => {
  await page.screenshot({ path: `${out}fr-upgrade-failed.png` });
  console.log(`FAIL ${message}: ${new URL(page.url()).pathname} ${(await page.locator('body').innerText()).slice(0, 400).replace(/\s+/g, ' ')}`);
  await browser.close();
  process.exit(1);
};

await page.goto(`${base}/login`);
await page.fill('input[name=email]', 'first-root@example.test');
await page.fill('input[name=password]', password);
await page.click('button[type=submit]');
await page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 20000 }).catch(() => {});
await page.goto(`${base}/settings/updates`);
await page.waitForTimeout(2500);
const check = page.getByRole('button', { name: /Check/ }).first();
await check.click().catch(() => {});
await page.waitForTimeout(3000);
await page.screenshot({ path: `${out}fr-upgrade-available.png` });
const open = page.getByRole('button', { name: 'Upgrade now' }).first();
if (!(await open.count())) await fail('no Upgrade now button');
await open.click();
await page.waitForTimeout(500);
await page.screenshot({ path: `${out}fr-upgrade-confirm.png` });
await page.getByRole('button', { name: 'Upgrade now' }).last().click();
await page.waitForTimeout(4000);
await page.screenshot({ path: `${out}fr-upgrade-progress.png` });
console.log('progress view:', (await page.locator('h3').filter({ hasText: /Upgrad/ }).first().innerText().catch(() => '')).trim());
page.on('dialog', (d) => d.accept().catch(() => {}));
const deadline = Date.now() + 240000;
let finished = false;
while (Date.now() < deadline) {
  const text = await page.locator('body').innerText().catch(() => '');
  if (text.includes('Upgrade complete') || text.includes('Successfully upgraded')) {
    finished = true;
    await page.screenshot({ path: `${out}fr-upgrade-complete.png` });
    break;
  }
  if (text.includes('Error:')) await fail('the progress view reports an error');
  await page.waitForTimeout(2000);
}
if (!finished) await fail('the progress view did not finish');
console.log('progress view finished:', target);
await browser.close();
