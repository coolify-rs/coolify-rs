// Screens of the service and application backup pages on both sides, with the rows of backups_fixture.py.
//   node flow_backups.mjs --rs=18087 --fixture=/path/fixture.json [--light] [--only=prefix]
// The test password is read from the fixture file and never printed; errors are redacted.
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const args = process.argv.slice(2);
const opt = (k, v) => (args.find((a) => a.startsWith(`--${k}=`)) ?? `=${v}`).split('=')[1];
const rsPort = opt('rs', 18087);
const fx = JSON.parse(fs.readFileSync(opt('fixture', ''), 'utf8'));
const only = opt('only', '');
const light = args.includes('--light');
const secret = fs.readFileSync(new URL('../reference/secrets/ui-login.txt', import.meta.url), 'utf8').match(/^password:\s*(.+)$/m)[1].trim();
const redact = (s) => String(s).split(secret).join('***');
const out = new URL('./shots/', import.meta.url).pathname;
fs.mkdirSync(out, { recursive: true });
const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });

async function session(base) {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: light ? 'light' : 'dark' });
  const page = await ctx.newPage();
  await page.addInitScript((l) => {
    try {
      localStorage.setItem('theme', l ? 'light' : 'dark');
    } catch {}
  }, light);
  await page.goto(base + '/login');
  await page.fill('input[name=email]', 'alice@example.test');
  await page.fill('input[name=password]', secret);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 }).catch(() => {}), page.click('button[type=submit]')]);
  return page;
}

const root = `/project/${fx.project}/environment/${fx.env}`;
const svc = `${root}/service/${fx.svc}`;
const app = `${root}/application/${fx.app}`;
const closeModal = (p) => p.keyboard.press('Escape');
const button = (p, name) => p.getByRole('button', { name, exact: true }).first();

const steps = [
  ['svc-empty', `${root}/service/${fx.svc_none}/storage-backups`],
  ['svc-list', `${svc}/storage-backups`],
  ['svc-menu', `${svc}/storage-backups`, async (p) => button(p, 'Add backup').click()],
  ['svc-new-storage', `${svc}/storage-backups`, async (p) => {
    await button(p, 'Add backup').click();
    await p.getByRole('menuitem', { name: 'Storage backup' }).first().click();
  }],
  ['svc-new-db', `${svc}/storage-backups`, async (p) => {
    await button(p, 'Add backup').click();
    await p.getByRole('menuitem', { name: 'Database backup' }).first().click();
  }],
  ['svc-filter', `${svc}/storage-backups`, async (p) => button(p, 'Filter').click()],
  ['svc-sort', `${svc}/storage-backups`, async (p) => button(p, 'Sort').click()],
  ['svc-noresults', `${svc}/storage-backups`, async (p) => p.fill('input[aria-label="Search backups"]', 'zzzz')],
  ['svc-modal-vol', `${svc}/storage-backups?backup_uuid=${fx.web_backup}`],
  ['svc-modal-vol-danger', `${svc}/storage-backups?backup_uuid=${fx.web_backup}`, async (p) => button(p, 'Danger Zone').click()],
  ['svc-modal-db', `${svc}/storage-backups?backup_uuid=${fx.db_backup}`],
  ['svc-modal-db-danger', `${svc}/storage-backups?backup_uuid=${fx.db_backup}`, async (p) => button(p, 'Danger Zone').click()],
  ['svc-exec-modal', `${svc}/storage-backups`, async (p) => p.locator('.data-table-row', { hasText: 'postgres' }).or(p.locator('.data-table-row', { hasText: 'Database' })).last().click()],
  ['svc-storages', `${svc}/storages`],
  ['imp-one', `${svc}/import-backup`],
  ['imp-none', `${root}/service/${fx.svc_none}/import-backup`],
  ['imp-two', `${root}/service/${fx.svc_two}/import-backup`],
  ['app-empty', `${root}/application/${fx.app_empty}/backups`],
  ['app-list', `${app}/backups`],
  ['app-new', `${app}/backups`, async (p) => button(p, 'Add').click()],
  ['app-noresults', `${app}/backups`, async (p) => p.fill('input[aria-label="Search backups"]', 'zzzz')],
  ['app-general', `${app}/backups/${fx.app_backup}`],
  ['app-exec', `${app}/backups/${fx.app_backup}/executions`],
  ['app-danger', `${app}/backups/${fx.app_backup}/danger`],
  ['app-storages', `${app}/persistent-storage`],
  ['app-storages-modal', `${app}/persistent-storage`, async (p) => p.locator('.volumes-col-actions button', { hasText: 'Backup' }).last().click()],
];

// Where each of these ends up; nothing is drawn for them upstream.
const redirects = [
  `${svc}/storage-backups/${fx.web_backup}`,
  `${svc}/storage-backups/${fx.web_backup}/executions`,
  `${svc}/storage-backups/${fx.web_backup}/danger`,
  `${svc}/storage-backups/${fx.web_backup}/s3`,
  `${svc}/storage-backups/${fx.web_backup}/retention`,
  `${svc}/${fx.database}/backups`,
  `${svc}/${fx.database}/backups/${fx.db_backup}`,
  `${svc}/${fx.database}/backups/${fx.db_backup}/executions`,
  `${svc}/${fx.database}/backups/${fx.db_backup}/danger`,
  `${svc}/${fx.database}/backups/${fx.db_backup}/s3`,
  `${svc}/${fx.database}/backups/${fx.db_backup}/retention`,
  `${svc}/nope/backups`,
  `${svc}/${fx.database}/import`,
  `${svc}/import-backup`,
  `${app}/backups/${fx.app_backup}/s3`,
  `${app}/backups/${fx.app_backup}/retention`,
];

async function settle(page) {
  await page.waitForTimeout(2200);
  for (let i = 0; i < 2; i++) {
    await page.locator('button:has-text("Accept And Close"), button:has-text("Accept and Close"), button:has-text("Maybe next time")').first().click({ timeout: 400 }).catch(() => {});
    await page.waitForTimeout(150);
  }
}

try {
  const sides = [
    ['ref', await session('http://127.0.0.1:18000'), 'http://127.0.0.1:18000'],
    ['rs', await session(`http://127.0.0.1:${rsPort}`), `http://127.0.0.1:${rsPort}`],
  ];
  const suffix = light ? '-light' : '';
  for (const [name, path, act] of steps) {
    if (only && !name.startsWith(only)) continue;
    for (const [label, page, base] of sides) {
      try {
        await page.goto(base + path);
        await settle(page);
        if (act) {
          await act(page);
          await page.waitForTimeout(900);
        }
      } catch (e) {
        console.log('STEP FAILED', label, name, redact(e.message).split('\n')[0]);
      }
      await page.screenshot({ path: `${out}bk-${name}${suffix}-${label}.png`, fullPage: false });
    }
    console.log(`bk-${name}${suffix}`);
    await closeModal(sides[0][1]).catch(() => {});
    await closeModal(sides[1][1]).catch(() => {});
  }
  if (!only && !light) {
    let same = 0;
    for (const path of redirects) {
      const ends = [];
      for (const [, page, base] of sides) {
        const r = await page.goto(base + path);
        await page.waitForTimeout(1800);
        ends.push(`${new URL(page.url()).pathname}${new URL(page.url()).search} [${r?.status()}]`);
      }
      const a = ends[0].split(' ')[0];
      const b = ends[1].split(' ')[0];
      if (a === b) same++;
      else console.log('REDIRECT DIFF', path, '\n  ref', ends[0], '\n  rs ', ends[1]);
    }
    console.log(`redirects ${same}/${redirects.length}`);
  }
} catch (e) {
  console.log('FAILED', redact(e.message));
} finally {
  await browser.close();
}
