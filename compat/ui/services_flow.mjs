// Runs the same UI steps on both sides, one step at a time, with a screenshot after each.
//   node services_flow.mjs --rs=18086 <project> <environment> <ref service> <rs service> <step> [step ...]
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const args = process.argv.slice(2);
const rsPort = (args.find((a) => a.startsWith('--rs=')) ?? '--rs=18086').split('=')[1];
const [project, env, refService, rsService, ...steps] = args.filter((a) => !a.startsWith('--'));
const secret = fs
  .readFileSync(new URL('../reference/secrets/ui-login.txt', import.meta.url), 'utf8')
  .match(/^password:\s*(.+)$/m)[1]
  .trim();
const out = new URL('./shots/', import.meta.url).pathname;
const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });

async function session(base) {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 1300 }, colorScheme: 'dark' });
  const page = await ctx.newPage();
  await page.addInitScript(() => {
    try {
      localStorage.setItem('theme', 'dark');
    } catch {}
  });
  await page.goto(base + '/login');
  await page.fill('input[name=email]', 'alice@example.test');
  await page.fill('input[name=password]', secret);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 }).catch(() => {}), page.keyboard.press('Enter')]);
  await page.waitForTimeout(800);
  return page;
}

const visible = (p, name) => p.getByRole('button', { name, exact: true }).locator('visible=true').last();

async function confirm(p, { text, password, cont } = {}) {
  await p.waitForTimeout(400);
  if (cont) {
    await visible(p, 'Continue').click();
    await p.waitForTimeout(300);
  }
  if (text) {
    await p.locator('input:visible').last().fill(text);
    await visible(p, 'Continue').click();
    await p.waitForTimeout(300);
  }
  if (password) {
    await p.locator('input[type=password]:visible').last().fill(secret);
  }
}

const stepsImpl = {
  rename: async (p) => {
    await p.locator('input[id="name"], input[wire\\:model="name"]').first().fill('flow service');
    await p.locator('input[id="description"], input[wire\\:model="description"]').first().fill('flow description');
    await p.locator('input[id="description"], input[wire\\:model="description"]').first().press('Enter');
    await p.waitForTimeout(3000);
  },
  badname: async (p) => {
    await p.locator('input[id="name"], input[wire\\:model="name"]').first().fill('x<>');
    await p.locator('input[id="name"], input[wire\\:model="name"]').first().press('Enter');
    await p.waitForTimeout(1500);
  },
  deploy: async (p) => {
    await p.locator('.split-action-main:visible').first().click();
    await p.waitForTimeout(25000);
  },
  dialogclose: async (p) => {
    await p.keyboard.press('Escape');
  },
  stopmodal: async (p) => {
    await p.locator('.split-action-caret:visible').first().click();
    await p.waitForTimeout(300);
    await p.getByRole('menuitem', { name: 'Stop' }).locator('visible=true').first().click();
    await p.waitForTimeout(800);
  },
  stop: async (p) => {
    await p.locator('.split-action-caret:visible').first().click();
    await p.waitForTimeout(300);
    await p.getByRole('menuitem', { name: 'Stop' }).locator('visible=true').first().click();
    await p.waitForTimeout(800);
    // Docker cleanup stays off: the VM is shared.
    await p.getByText('Run Docker Cleanup', { exact: false }).locator('visible=true').first().click().catch(() => {});
    await confirm(p, { cont: true });
    await visible(p, 'Confirm').click();
    await p.waitForTimeout(20000);
  },
  menu: async (p) => {
    await p.locator('.split-action-caret:visible').first().click();
    await p.waitForTimeout(500);
  },
  files: async (p, base) => {
    await p.goto(base + '/storages');
    await p.waitForTimeout(2500);
    await p.getByRole('button', { name: /^Files \(/ }).first().click();
    await p.waitForTimeout(1500);
  },
  loadfile: async (p) => {
    await p.getByRole('button', { name: 'Load from server' }).first().click();
    await p.waitForTimeout(3000);
  },
  dirs: async (p, base) => {
    await p.goto(base + '/storages');
    await p.waitForTimeout(2500);
    await p.getByRole('button', { name: /^Directories \(/ }).first().click();
    await p.waitForTimeout(1500);
  },
  home: async (p, base) => {
    await p.goto(base);
    await p.waitForTimeout(2500);
  },
  convertmodal: async (p) => {
    await p.locator('[aria-label="Resource settings"]:visible').first().click();
    await p.waitForTimeout(2500);
    await visible(p, 'Convert to Database').click();
    await p.waitForTimeout(800);
  },
};

const sides = [
  ['ref', await session('http://127.0.0.1:18000'), `http://127.0.0.1:18000/project/${project}/environment/${env}/service/${refService}`],
  ['rs', await session(`http://127.0.0.1:${rsPort}`), `http://127.0.0.1:${rsPort}/project/${project}/environment/${env}/service/${rsService}`],
];
for (const [, page, base] of sides) {
  await page.goto(base);
  await page.waitForTimeout(2500);
}
let n = 0;
for (const step of steps) {
  n++;
  await Promise.all(
    sides.map(async ([label, page, base]) => {
      try {
        await stepsImpl[step](page, base);
      } catch (e) {
        console.log(step, label, 'failed:', String(e).split('\n')[0].slice(0, 200));
      }
      await page.screenshot({ path: `${out}svcflow-${step}-${label}.png` });
    }),
  );
  console.log('done', step);
}
await browser.close();
