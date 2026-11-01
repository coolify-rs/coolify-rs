// Upstream's image running on a database created by coolify-rs: the first user signs in and sees the data.
//   node install_adopted.mjs <tunnel port> <password file>
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const [port, passFile] = process.argv.slice(2);
const base = `http://127.0.0.1:${port}`;
const out = new URL('./shots/', import.meta.url).pathname;
const browser = await chromium.launch({
  executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  headless: true,
});
const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: 'dark' });
const page = await ctx.newPage();
await page.goto(`${base}/login`);
await page.fill('input[name=email]', 'first-root@example.test');
await page.fill('input[name=password]', fs.readFileSync(passFile, 'utf8'));
await page.click('button[type=submit]');
await page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 30000 }).catch(() => {});
await page.waitForTimeout(2500);
console.log('signed in, at', new URL(page.url()).pathname);
// The onboarding page stands in front of every page until it is finished or skipped.
if (new URL(page.url()).pathname === '/onboarding') {
  await page.waitForTimeout(1500);
  await page.getByRole('button', { name: 'Skip setup' }).first().click().catch(() => {});
  await page.waitForTimeout(2500);
}
await page.goto(`${base}/servers`);
await page.waitForTimeout(2500);
await page.screenshot({ path: `${out}fr-adopted-servers.png` });
const servers = await page.locator('body').innerText();
console.log('servers page lists localhost:', servers.includes('localhost'));
await page.goto(`${base}/projects`);
await page.waitForTimeout(2500);
await page.screenshot({ path: `${out}fr-adopted-projects.png` });
const projects = await page.locator('body').innerText();
console.log('projects page lists the project:', projects.includes('install check'));
await browser.close();
process.exit(servers.includes('localhost') && projects.includes('install check') ? 0 : 1);
