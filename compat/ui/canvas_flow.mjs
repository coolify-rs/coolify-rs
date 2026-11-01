// The remembered view (RemembersCanvasView) and the toolbar links on both sides, read-only.
//   node canvas_flow.mjs [--rs=18092]
// Each step prints the path both sides ended on, and whether they agree.
// The test password is read from the fixture file and never printed.
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const args = process.argv.slice(2);
const port = (args.find((a) => a.startsWith('--rs=')) ?? '--rs=18092').split('=')[1];
const P = 'j8xm2emlllhfnyebjzgyzp7z';
const E = 'uj3st9yvtvzxh7rul3c3opcz';
const S = 'hml20fsj5eles0xfiqidq8j5';
const secret = fs.readFileSync(new URL('../reference/secrets/ui-login.txt', import.meta.url), 'utf8').match(/^password:\s*(.*)$/m)[1].trim();
const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });

const steps = [
  ['/projects/canvas', '/projects'],
  [`/project/${P}/canvas`, `/project/${P}`],
  [`/project/${P}/environment/${E}/canvas`, `/project/${P}/environment/${E}`],
  ['/servers/canvas', '/servers'],
  [`/server/${S}/canvas`, `/server/${S}/resources`],
  ['/dashboard/canvas', '/'],
  ['/canvas/tags', '/tags'],
  ['/shared-variables/team/canvas', '/shared-variables/team'],
  [`/shared-variables/project/${P}/canvas`, `/shared-variables/project/${P}`],
  [`/shared-variables/server/${S}/canvas`, `/shared-variables/server/${S}`],
];

async function walk(base) {
  const ctx = await browser.newContext({ colorScheme: 'dark', viewport: { width: 1440, height: 900 } });
  const page = await ctx.newPage();
  await page.goto(`${base}/login`);
  await page.fill('input[name=email]', 'alice@example.test');
  await page.fill('input[name=password]', secret);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 }).catch(() => {}), page.keyboard.press('Enter')]);
  const out = [];
  const settle = async () => {
    await page.waitForTimeout(1500);
    return new URL(page.url()).pathname;
  };
  for (const [canvas, list] of steps) {
    await page.goto(base + canvas);
    const onCanvas = await settle();
    await page.goto(base + list);
    const reopened = await settle();
    await page.goto(base + canvas);
    await settle();
    await page.locator('a[title="List view"]').first().click({ timeout: 5000 }).catch(() => {});
    const backTo = await settle();
    const search = new URL(page.url()).search;
    await page.goto(base + list);
    const stays = await settle();
    out.push({ canvas, onCanvas, reopened, backTo, search, stays });
  }
  await ctx.close();
  return out;
}

const [ref, rs] = [await walk('http://127.0.0.1:18000'), await walk(`http://127.0.0.1:${port}`)];
await browser.close();
let same = 0;
for (let i = 0; i < ref.length; i++) {
  if (JSON.stringify(ref[i]) === JSON.stringify(rs[i])) same++;
  else console.log(`DIFF\n  ref ${JSON.stringify(ref[i])}\n  rs  ${JSON.stringify(rs[i])}`);
}
console.log(`same ${same}, different ${ref.length - same}`);
process.exit(same === ref.length ? 0 : 1);
