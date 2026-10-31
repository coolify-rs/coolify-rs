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
  domains: async (p, base) => {
    await p.goto(base + '/domains');
    await p.waitForTimeout(2500);
  },
  domainedit: async (p) => {
    await p.locator('[aria-label^="Settings for "]:visible').first().click();
    await p.waitForTimeout(1000);
  },
  domainadd: async (p) => {
    await p.getByRole('button', { name: 'Add domain' }).locator('visible=true').first().click();
    await p.waitForTimeout(700);
    await p.locator('#newDomainParts-host').fill('app.flow-test.example.test');
    await p.waitForTimeout(300);
    await p.keyboard.press('Enter');
    await p.waitForTimeout(3000);
  },
  domainportforce: async (p) => {
    await p.getByRole('button', { name: 'Use this port anyway' }).click();
    await p.waitForTimeout(3000);
  },
  proceed: async (p) => {
    const b = p.getByRole('button', { name: 'Proceed anyway' });
    if (await b.count()) await b.first().click();
    await p.waitForTimeout(3000);
  },
  port: async (p) => {
    const b = p.getByRole('button', { name: 'Use this port anyway' });
    if (await b.count()) await b.first().click();
    await p.waitForTimeout(3000);
  },
  domaineditsave: async (p) => {
    await p.locator('[aria-label^="Settings for "]:visible').last().click();
    await p.waitForTimeout(800);
    await p.locator('#editingDomainParts-host').fill('app2.flow-test.example.test');
    await p.locator('#editingDomainParts-port').fill('8123');
    await p.waitForTimeout(300);
    await visible(p, 'Save').click();
    await p.waitForTimeout(3000);
  },
  domainremove: async (p) => {
    await p.locator('[aria-label="Remove domain"]:visible').last().click();
    await p.waitForTimeout(600);
    await visible(p, 'Continue').click();
    await p.waitForTimeout(400);
    await visible(p, 'Remove domain').click();
    await p.waitForTimeout(3000);
  },
  partedit: async (p) => {
    await p.locator('[aria-label="Resource settings"]:visible').first().click();
    await p.waitForTimeout(2500);
    await p.locator('input#humanName, input[wire\\:model="humanName"]').locator('visible=true').first().fill('Part human name');
    await p.locator('input#description, input[wire\\:model="description"]').locator('visible=true').last().fill('Part description');
    await p.locator('input#maxRestartCount, input[wire\\:model="maxRestartCount"]').locator('visible=true').first().fill('7');
    await visible(p, 'Save changes').click();
    await p.waitForTimeout(3000);
  },
  partconvert: async (p) => {
    await p.locator('[aria-label="Resource settings"]:visible').first().click();
    await p.waitForTimeout(2500);
    await visible(p, 'Convert to Database').click();
    await p.waitForTimeout(800);
    await p.locator('input:visible').last().fill('Filebrowser');
    await visible(p, 'Continue').click();
    await p.waitForTimeout(500);
    await p.locator('input[type=password]:visible').last().fill(secret);
    await visible(p, 'Confirm').dispatchEvent('click');
    await p.waitForTimeout(4000);
  },
  dbdelete: async (p) => {
    await p.locator('[aria-label="Resource settings"]:visible').last().click();
    await p.waitForTimeout(2500);
    await visible(p, 'Delete').click();
    await p.waitForTimeout(800);
    await p.locator('input:visible').last().fill('Redis');
    await visible(p, 'Continue').click();
    await p.waitForTimeout(500);
    await p.locator('input[type=password]:visible').last().fill(secret);
    await visible(p, 'Confirm').dispatchEvent('click');
    await p.waitForTimeout(4000);
  },
  composeedit: async (p) => {
    await p.getByRole('button', { name: 'Edit Compose file' }).first().click();
    await p.waitForTimeout(3000);
    await p.getByText('Use plain-text editor', { exact: false }).locator('visible=true').first().click();
    await p.waitForTimeout(500);
    const area = p.locator('textarea:visible').first();
    const value = await area.inputValue();
    await area.fill(value.replace('services:', 'services:\n  extra:\n    image: nginx:alpine'));
    await p.waitForTimeout(500);
    await visible(p, 'Save changes').dispatchEvent('click');
    await p.waitForTimeout(9000);
  },
  partsave: async (p) => {
    await p.locator('[aria-label="Resource settings"]:visible').first().click();
    await p.waitForTimeout(2500);
    await visible(p, 'Save changes').dispatchEvent('click');
    await p.waitForTimeout(4000);
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
  const run = async ([label, page, base]) => {
    try {
      await stepsImpl[step](page, base);
    } catch (e) {
      console.log(step, label, 'failed:', String(e).split('\n')[0].slice(0, 200));
    }
    await page.screenshot({ path: `${out}svcflow-${step}-${label}.png` });
  };
  // Long waits run side by side; domain steps one side at a time, since they see each other's rows.
  if (step === 'deploy' || step === 'stop') await Promise.all(sides.map(run));
  else for (const side of sides) await run(side);
  console.log('done', step);
}
await browser.close();
