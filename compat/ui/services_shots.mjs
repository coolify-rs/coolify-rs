// Screenshots of service page interactions on both sides.
//   node services_shots.mjs --rs=18086 [--light] <project> <environment> <ref service> <rs service> [scene ...]
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const args = process.argv.slice(2);
const rsPort = (args.find((a) => a.startsWith('--rs=')) ?? '--rs=18086').split('=')[1];
const light = args.includes('--light');
const [project, env, refService, rsService, ...only] = args.filter((a) => !a.startsWith('--'));
const secret = fs
  .readFileSync(new URL('../reference/secrets/ui-login.txt', import.meta.url), 'utf8')
  .match(/^password:\s*(.+)$/m)[1]
  .trim();
const out = new URL('./shots/', import.meta.url).pathname;
const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });

async function session(base) {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 1300 }, colorScheme: light ? 'light' : 'dark' });
  const page = await ctx.newPage();
  await page.addInitScript((l) => {
    try {
      localStorage.setItem('theme', l ? 'light' : 'dark');
    } catch {}
  }, light);
  await page.goto(base + '/login');
  await page.fill('input[name=email]', 'alice@example.test');
  await page.fill('input[name=password]', secret);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 }).catch(() => {}), page.keyboard.press('Enter')]);
  await page.waitForTimeout(800);
  return page;
}

const suffix = light ? '-light' : '';
const scenes = {
  // The settings modal of the first compose resource row.
  modal: async (p) => {
    await p.locator('[aria-label="Resource settings"]:visible').first().click();
    await p.waitForTimeout(2500);
  },
  dbmodal: async (p) => {
    await p.locator('[aria-label="Resource settings"]:visible').nth(1).click();
    await p.waitForTimeout(2500);
  },
  compose: async (p) => {
    await p.getByRole('button', { name: 'Edit Compose file' }).first().click();
    await p.waitForTimeout(3500);
  },
  grid: async (p) => {
    await p.locator('[title="Grid view"]:visible, [aria-label="Grid view"]:visible').first().click();
    await p.waitForTimeout(800);
  },
  links: async (p) => {
    await p.locator('[title="Open service links"]:visible').first().click();
    await p.waitForTimeout(500);
  },
  actions: async (p) => {
    await p.locator('[aria-label="More actions"]:visible').first().click();
    await p.waitForTimeout(500);
  },
  stop: async (p) => {
    await p.locator('[aria-label="More actions"]:visible').first().click();
    await p.waitForTimeout(300);
    await p.getByRole('menuitem', { name: 'Stop' }).locator('visible=true').first().click();
    await p.waitForTimeout(800);
  },
};

const sides = [
  ['ref', await session('http://127.0.0.1:18000'), 'http://127.0.0.1:18000', refService],
  ['rs', await session(`http://127.0.0.1:${rsPort}`), `http://127.0.0.1:${rsPort}`, rsService],
];
for (const name of only.length ? only : Object.keys(scenes)) {
  for (const [label, page, base, service] of sides) {
    await page.goto(`${base}/project/${project}/environment/${env}/service/${service}`);
    await page.waitForTimeout(2500);
    try {
      await scenes[name](page);
    } catch (e) {
      console.log(name, label, 'failed:', String(e).slice(0, 200));
    }
    await page.screenshot({ path: `${out}svc-${name}${suffix}-${label}.png` });
    console.log(`svc-${name}${suffix}-${label}`);
  }
}
await browser.close();
