// Creates a one-click service through both UIs and prints the two uuids, for compat/diff/services_ui_diff.py.
//   node services_create.mjs --rs=18086 <template> <project> <environment> <destination> <server name>
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const args = process.argv.slice(2);
const rsPort = (args.find((a) => a.startsWith('--rs=')) ?? '--rs=18086').split('=')[1];
const [template, project, env, destination, serverName] = args.filter((a) => !a.startsWith('--'));
const secret = fs
  .readFileSync(new URL('../reference/secrets/ui-login.txt', import.meta.url), 'utf8')
  .match(/^password:\s*(.+)$/m)[1]
  .trim();
const browser = await chromium.launch({
  executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  headless: true,
});

async function session(base) {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const page = await ctx.newPage();
  await page.goto(base + '/login');
  await page.fill('input[name=email]', 'alice@example.test');
  await page.fill('input[name=password]', secret);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 }).catch(() => {}), page.keyboard.press('Enter')]);
  return page;
}

const out = {};
const ref = await session('http://127.0.0.1:18000');
await ref.goto(`http://127.0.0.1:18000/project/${project}/environment/${env}/new?type=one-click-service-${template}&destination=${destination}`);
await ref.waitForURL(/\/service\//, { timeout: 60000 });
out.ref = ref.url().match(/service\/([^/?]+)/)[1];

const rs = await session(`http://127.0.0.1:${rsPort}`);
await rs.goto(`http://127.0.0.1:${rsPort}/project/${project}/environment/${env}/new`);
await rs.waitForSelector('article[aria-label^="Deploy "]', { timeout: 30000 });
await rs.fill('input[type=search]', template.replace(/-/g, ' ').split(' ')[0]);
const card = rs.locator(`article[aria-label^="Deploy "]`, { hasText: new RegExp(template.split('-').join('.*'), 'i') }).first();
await card.click();
for (let i = 0; i < 40 && !/\/service\//.test(rs.url()); i++) {
  const server = rs.getByText(serverName, { exact: true });
  if (await server.count()) await server.first().click().catch(() => {});
  await rs.waitForTimeout(500);
}
await rs.waitForURL(/\/service\//, { timeout: 60000 });
out.rs = rs.url().match(/service\/([^/?]+)/)[1];
console.log(JSON.stringify(out));
await browser.close();
