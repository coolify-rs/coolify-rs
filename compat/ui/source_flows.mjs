// The browser flows of the sources pages on both UIs against compat/diff/fake_git_provider.py:
// create through the modal, the GitHub App manifest flow with the install callback, the GitLab and
// Gitea OAuth flows, then compares the rows. Proves the flows against the fake only.
//   node source_flows.mjs --rs=18085 --fake=http://<address>:18990
import fs from 'node:fs';
import { execFileSync } from 'node:child_process';
import { chromium } from 'playwright-core';

const args = process.argv.slice(2);
const opt = (k, d) => (args.find((a) => a.startsWith(`--${k}=`)) ?? `=${d}`).split('=')[1];
const rs = opt('rs', '18085');
const fake = args.find((a) => a.startsWith('--fake=')).slice('--fake='.length);
const secret = fs.readFileSync(new URL('../reference/secrets/ui-login.txt', import.meta.url), 'utf8').match(/^password:\s*(.+)$/m)[1].trim();
const out = new URL('./shots/', import.meta.url).pathname;
const sql = (q) => execFileSync('docker', ['exec', 'coolify-rs-reference-postgres-1', 'psql', '-U', 'coolify', '-d', 'coolify', '-At', '-c', q]).toString().trim();

const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
async function login(base) {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 1000 }, colorScheme: 'dark' });
  const page = await ctx.newPage();
  await page.goto(base + '/login');
  await page.fill('input[name=email]', 'alice@example.test');
  await page.fill('input[name=password]', secret);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 }).catch(() => {}), page.click('button[type=submit]')]);
  return page;
}
const dismiss = (page) => page.locator('button:has-text("Accept And Close"), button:has-text("Maybe next time")').first().click({ timeout: 500 }).catch(() => {});
const field = (page, label) => page.locator(`xpath=//label[contains(normalize-space(.), "${label}")]/following::input[1]`).locator('visible=true').first();

const flows = {
  github: { menu: 'GitHub App', html: `${fake}/gh`, api: `${fake}/github`, advanced: 'Self-hosted / Enterprise GitHub', urlLabel: 'HTML URL' },
  gitlab: { menu: 'GitLab App', html: `${fake}/gl`, api: `${fake}/gitlab/api/v4`, advanced: 'Self-hosted GitLab', urlLabel: 'GitLab URL' },
  gitea: { menu: 'Gitea App', html: `${fake}/gt`, api: `${fake}/gitea/api/v1`, advanced: 'Self-hosted Gitea', urlLabel: 'Gitea URL' },
};

async function createSource(page, base, provider, name, shot) {
  const f = flows[provider];
  await page.goto(`${base}/sources`);
  await page.waitForTimeout(1500);
  await dismiss(page);
  await page.locator('button:has-text("New Source")').first().dispatchEvent('click');
  await page.locator(`button:has-text("${f.menu}")`).locator('visible=true').first().dispatchEvent('click');
  await page.waitForTimeout(800);
  await page.screenshot({ path: `${out}${shot}-modal.png` });
  await field(page, 'Name').fill(name);
  await page.locator(`button:has-text("${f.advanced}")`).locator('visible=true').first().click({ force: true });
  await page.waitForTimeout(500);
  await page.screenshot({ path: `${out}${shot}-modal-open.png` });
  await field(page, f.urlLabel).fill(f.html);
  await field(page, f.urlLabel).blur();
  await page.waitForTimeout(1200);
  await page.locator('button:has-text("Continue")').locator('visible=true').last().dispatchEvent('click');
  await page.waitForURL(new RegExp(`/source/${provider}/[a-z0-9]+`), { timeout: 30000 });
  await dismiss(page);
  await page.waitForTimeout(1200);
  return page.url().match(/\/source\/[a-z]+\/([a-z0-9]+)/)[1];
}

async function customEndpoint(page, base) {
  await page.locator('button:has-text("Use an instance endpoint")').first().dispatchEvent('click');
  await page.locator('[role=option]:has-text("Use a custom endpoint"), .listbox-option:has-text("Use a custom endpoint")').first().dispatchEvent('click');
  await page.waitForTimeout(300);
  await page.fill('input[placeholder="https://coolify.example.com"]', base);
  await page.locator('input[placeholder="https://coolify.example.com"]').blur();
  await page.waitForTimeout(800);
}

function rowOf(table, name, hide) {
  const t = JSON.parse(sql(`SELECT row_to_json(s) FROM ${table} s WHERE name = '${name}'`) || 'null');
  if (!t) return null;
  for (const k of ['id', 'uuid', 'created_at', 'updated_at', 'name', 'expires_at', 'private_key_id', 'redirect_uri']) delete t[k];
  for (const k of hide) t[k] = !!t[k];
  return t;
}

const sides = [['ref', 'http://127.0.0.1:18000'], ['rs', `http://127.0.0.1:${rs}`]];
const pages = {};
for (const [label, base] of sides) pages[label] = await login(base);
let failed = 0;
const compare = (name, a, b) => {
  const same = !a?.error && !b?.error && JSON.stringify(a) === JSON.stringify(b);
  console.log(`${same ? 'PASS' : 'FAIL'} ${name}`);
  if (!same) {
    failed++;
    console.log('  ref', JSON.stringify(a));
    console.log('  rs ', JSON.stringify(b));
  }
};

// The GitHub App manifest flow.
{
  const rows = {};
  for (const [label, base] of sides) {
    const page = pages[label];
    const name = `flowgh${label}`;
    try {
      await createSource(page, base, 'github', name, `sf-gh-${label}`);
      await page.screenshot({ path: `${out}sf-gh-${label}-register.png`, fullPage: true });
      await customEndpoint(page, base);
      await page.locator('button:has-text("Register With GitHub")').first().dispatchEvent('click');
      await page.waitForURL(/\/source\/github\/[a-z0-9]+$/, { timeout: 30000 });
      await page.locator('a:has-text("Install Repositories")').first().waitFor({ timeout: 15000 });
      await page.screenshot({ path: `${out}sf-gh-${label}-install.png`, fullPage: true });
      rows[`${label}-registered`] = rowOf('github_apps', 'fake-app', ['client_secret', 'webhook_secret']);
      await page.locator('a:has-text("Install Repositories")').first().dispatchEvent('click');
      await page.waitForURL(/\/source\/github\/[a-z0-9]+$/, { timeout: 30000 });
      await page.waitForTimeout(1500);
      await page.screenshot({ path: `${out}sf-gh-${label}-done.png`, fullPage: true });
      rows[label] = rowOf('github_apps', 'fake-app', ['client_secret', 'webhook_secret']);
      rows[`${label}-key`] = sql(`SELECT name, is_git_related, description FROM private_keys WHERE name = 'github-app-fake-app' ORDER BY id DESC LIMIT 1`);
    } catch (e) {
      await page.screenshot({ path: `${out}sf-gh-${label}-error.png`, fullPage: true }).catch(() => {});
      rows[label] = { error: String(e).slice(0, 400) };
    }
    sql(`DELETE FROM github_apps WHERE name IN ('fake-app', '${name}')`);
    sql(`DELETE FROM private_keys WHERE name = 'github-app-fake-app'`);
  }
  compare('github manifest registered', rows['ref-registered'], rows['rs-registered']);
  compare('github installed', rows.ref, rows.rs);
  compare('github key', rows['ref-key'], rows['rs-key']);
}

// The OAuth flows.
for (const provider of ['gitlab', 'gitea']) {
  const rows = {};
  const table = `${provider}_apps`;
  const hide = ['client_secret', 'access_token', 'refresh_token', provider === 'gitlab' ? 'webhook_token' : 'webhook_secret'];
  for (const [label, base] of sides) {
    const page = pages[label];
    const name = `flow${provider}${label}`;
    try {
      await createSource(page, base, provider, name, `sf-${provider}-${label}`);
      await page.screenshot({ path: `${out}sf-${provider}-${label}-new.png`, fullPage: true });
      rows[`${label}-new`] = rowOf(table, name, hide);
      await field(page, provider === 'gitlab' ? 'Application ID' : 'Client ID').fill('fake-client');
      await field(page, provider === 'gitlab' ? 'Application secret' : 'Client secret').fill('fake-secret');
      await customEndpoint(page, base);
      await page.locator('button:has-text("Save")').first().dispatchEvent('click');
      await page.locator(`a:has-text("Connect To ${provider === 'gitlab' ? 'GitLab' : 'Gitea'}")`).first().waitFor({ timeout: 15000 });
      await page.waitForTimeout(800);
      await page.screenshot({ path: `${out}sf-${provider}-${label}-saved.png`, fullPage: true });
      await page.locator(`a:has-text("Connect To ${provider === 'gitlab' ? 'GitLab' : 'Gitea'}")`).first().dispatchEvent('click');
      await page.waitForURL(new RegExp(`/source/${provider}/[a-z0-9]+$`), { timeout: 30000 });
      await page.locator('h1').first().waitFor();
      await page.waitForTimeout(1500);
      await page.screenshot({ path: `${out}sf-${provider}-${label}-connected.png`, fullPage: true });
      rows[label] = rowOf(table, name, hide);
    } catch (e) {
      await page.screenshot({ path: `${out}sf-${provider}-${label}-error.png`, fullPage: true }).catch(() => {});
      rows[label] = { error: String(e).slice(0, 400) };
    }
    sql(`DELETE FROM ${table} WHERE name LIKE 'flow%'`);
  }
  compare(`${provider} created`, rows['ref-new'], rows['rs-new']);
  compare(`${provider} connected`, rows.ref, rows.rs);
}
await browser.close();
process.exit(failed ? 1 : 0);
