// Admin page in dev mode (`APP_ENV=local scripts/dev-server.sh 18092`) as root: search, impersonate,
// return. The reference runs in production, where /admin answers 403, so this has no reference side.
//   node admin_check.mjs [--rs=18092]
// The test password is read from the fixture file and never printed.
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const args = process.argv.slice(2);
const port = (args.find((a) => a.startsWith('--rs=')) ?? '--rs=18092').split('=')[1];
const base = `http://127.0.0.1:${port}`;
const secret = fs.readFileSync(new URL('../reference/secrets/ui-login.txt', import.meta.url), 'utf8').match(/^password:\s*(.*)$/m)[1].trim();
const shots = new URL('./shots/', import.meta.url).pathname;
const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
const results = [];
async function login(user) {
  const ctx = await browser.newContext({ colorScheme: 'dark', viewport: { width: 1440, height: 900 } });
  const page = await ctx.newPage();
  await page.addInitScript(() => localStorage.setItem('theme', 'dark'));
  await page.goto(`${base}/login`);
  await page.fill('input[name=email]', `${user}@example.test`);
  await page.fill('input[name=password]', secret);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 }).catch(() => {}), page.keyboard.press('Enter')]);
  return page;
}
const alice = await login('alice');
await alice.goto(`${base}/admin`);
await alice.waitForTimeout(800);
results.push(['alice gets 403', (await alice.locator('.error-code').innerText().catch(() => '')) === '403']);

const root = await login('root');
results.push(['root signed in', !new URL(root.url()).pathname.startsWith('/login')]);
await root.goto(`${base}/admin`);
await root.waitForTimeout(800);
await root.screenshot({ path: `${shots}admin-rs.png` });
await root.fill('input#search', 'alice@');
await root.keyboard.press('Enter');
await root.waitForTimeout(600);
await root.screenshot({ path: `${shots}admin-search-rs.png` });
results.push(['found alice', (await root.locator('.data-table-row').count()) === 1]);
await root.locator('button:has-text("Switch user")').click();
await root.waitForURL((u) => u.pathname === '/', { timeout: 8000 }).catch(() => {});
await root.waitForTimeout(800);
const session = await root.evaluate(async () => (await fetch('/ui/session')).json());
results.push(['now alice', session.user.email === 'alice@example.test' && session.features.impersonating === true]);
await root.goto(`${base}/admin`);
await root.waitForTimeout(800);
results.push(['impersonator keeps admin', (await root.locator('text=Impersonation is active').count()) === 1]);
await root.locator('button:has-text("Return to root user")').click();
await root.waitForTimeout(1200);
const back = await root.evaluate(async () => (await fetch('/ui/session')).json());
results.push(['back to root', back.user.email === 'root@example.test' && back.features.impersonating === false]);
await browser.close();
for (const [name, ok] of results) console.log(`${ok ? 'ok' : 'FAIL'} ${name}`);
process.exit(results.every(([, ok]) => ok) ? 0 : 1);
