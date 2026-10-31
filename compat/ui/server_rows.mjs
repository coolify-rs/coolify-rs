// Saves server settings through both UIs and prints the changed rows, then restores them.
//   node server_rows.mjs --rs=18084
// Touches only settings columns and a destination name of the test server, nothing on the VM.
import { chromium } from 'playwright-core';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';

const rsPort = (process.argv.find((a) => a.startsWith('--rs=')) ?? '--rs=18084').split('=')[1];
const uuid = 'hml20fsj5eles0xfiqidq8j5';
const destination = 'jjxfu7mlvsrgoxhwrqhrj4yd';
const secret = fs.readFileSync(new URL('../reference/secrets/ui-login.txt', import.meta.url), 'utf8').match(/^password:\s*(.+)$/m)[1].trim();
const sql = (q) => execFileSync('docker', ['exec', 'coolify-rs-reference-postgres-1', 'psql', '-U', 'coolify', '-d', 'coolify', '-At', '-c', q]).toString().trim();
const sid = sql(`select id from servers where uuid='${uuid}'`);
const COLS = 'concurrent_builds,dynamic_timeout,deployment_queue_limit,server_disk_usage_notification_threshold,server_disk_usage_notification_interval_hours,server_disk_usage_check_frequency,docker_cleanup_frequency,docker_cleanup_threshold';
const settings = () => sql(`select ${COLS} from server_settings where server_id=${sid}`);
const destName = () => sql(`select name from standalone_dockers where uuid='${destination}'`);

const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
async function login(base) {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: 'dark' });
  const page = await ctx.newPage();
  await page.goto(base + '/login');
  await page.fill('input[name=email]', 'alice@example.test');
  await page.fill('input[name=password]', secret);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 }).catch(() => {}), page.click('button[type=submit]')]);
  return page;
}
const field = (id) => `input[id="${id}"], input[name="${id}"], input[wire\\:model="${id}"]`;
const toast = async (page, text) => (await page.getByText(text).count()) > 0;

async function run(base) {
  const page = await login(base);
  const out = {};
  const set = async (id, value) => {
    await page.fill(field(id), value);
    await page.keyboard.press('Tab');
  };
  const save = async () => {
    // The unsaved bar answers Enter from a form field.
    await page.locator(field(lastField)).first().press('Enter');
    await page.waitForTimeout(1500);
  };
  let lastField = 'concurrentBuilds';
  await page.goto(`${base}/server/${uuid}/advanced`);
  await page.waitForSelector(field('concurrentBuilds'));
  await page.waitForTimeout(1000);
  await set('concurrentBuilds', '7');
  await set('deploymentQueueLimit', '31');
  await save();
  out.advanced = settings();
  out.advancedToast = await toast(page, 'Server updated.');
  // Validation: threshold out of range, frequency invalid.
  await page.fill(field('serverDiskUsageNotificationThreshold'), '120');
  await save();
  out.thresholdError = [...new Set((await page.locator('text=/threshold/i').allInnerTexts()).filter((t) => /must/i.test(t)))].join(' | ');
  await page.goto(`${base}/server/${uuid}/advanced`);
  await page.waitForSelector(field('concurrentBuilds'));
  await page.waitForTimeout(1000);
  await page.fill(field('serverDiskUsageCheckFrequency'), 'not a cron');
  await page.keyboard.press('Tab');
  await save();
  out.cronError = await toast(page, 'Invalid Cron / Human expression for Disk Usage Check Frequency.');
  await page.goto(`${base}/destination/${destination}`);
  await page.waitForSelector(field('name'));
  await page.waitForTimeout(1000);
  await page.fill(field('name'), 'coolify-x');
  await page.locator(field('name')).first().press('Enter');
  await page.waitForTimeout(1500);
  out.destination = destName();
  out.destinationToast = await toast(page, 'Destination saved.');
  await page.context().close();
  return out;
}

const before = settings();
const beforeDest = destName();
const restore = () => {
  const [a, b, c, d, e, f, g, h] = before.split('|');
  sql(`update server_settings set concurrent_builds=${a},dynamic_timeout=${b},deployment_queue_limit=${c},server_disk_usage_notification_threshold=${d},server_disk_usage_notification_interval_hours=${e},server_disk_usage_check_frequency='${f}',docker_cleanup_frequency='${g}',docker_cleanup_threshold=${h} where server_id=${sid}`);
  sql(`update standalone_dockers set name='${beforeDest}' where uuid='${destination}'`);
};
let ref, rs;
try {
  ref = await run('http://127.0.0.1:18000');
  restore();
  rs = await run(`http://127.0.0.1:${rsPort}`);
} finally {
  restore();
  await browser.close();
}
let failed = 0;
for (const k of Object.keys(ref)) {
  const same = ref[k] === rs[k];
  if (!same) failed++;
  console.log(`${same ? 'ok  ' : 'FAIL'} ${k}\n     ref: ${ref[k]}\n     rs:  ${rs[k]}`);
}
console.log(`restored: ${settings() === before && destName() === beforeDest}`);
process.exit(failed ? 1 : 0);
