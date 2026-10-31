// Drives the private repository forms on both UIs against the fake provider and compares the
// application rows they create. Needs compat/ui/sources_fixture.py up and fake_git_provider.py.
//   node private_forms.mjs --rs=18085 --dest=<destination uuid> [--only=github,gitlab]
import fs from 'node:fs';
import { execFileSync } from 'node:child_process';
import { chromium } from 'playwright-core';

const args = process.argv.slice(2);
const opt = (k, d) => (args.find((a) => a.startsWith(`--${k}=`)) ?? `=${d}`).split('=')[1];
const rs = opt('rs', '18085');
const dest = opt('dest', '');
const only = opt('only', '').split(',').filter(Boolean);
const PROJECT = 'j8xm2emlllhfnyebjzgyzp7z';
const ENV = 'uj3st9yvtvzxh7rul3c3opcz';
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

async function repositoryForm(page, base, name, type, appName) {
  await page.goto(`${base}/project/${PROJECT}/environment/${ENV}/new?type=${type}&destination=${dest}`);
  await page.waitForTimeout(1500);
  await dismiss(page);
  await page.locator(`button:has-text("${appName}")`).first().click();
  await page.locator('button:has-text("Load Repository")').first().waitFor({ timeout: 30000 });
  await page.screenshot({ path: `${out}${name}-repo.png`, fullPage: true });
  await page.locator('button:has-text("Load Repository")').first().click();
  await page.locator('button:has-text("Continue")').first().waitFor({ timeout: 30000 });
  // The Smart Scan runs right after the branches load.
  await page.locator('text=/Dockerfile (selected|suggested)|Could not read|Docker Compose (selected|suggested)|No Dockerfile/').first().waitFor({ timeout: 90000 }).catch(() => {});
  await page.screenshot({ path: `${out}${name}-build.png`, fullPage: true });
  await page.locator('button:has-text("Continue")').first().click();
}

async function deployKeyForm(page, base, name) {
  await page.goto(`${base}/project/${PROJECT}/environment/${ENV}/new?type=private-deploy-key&destination=${dest}`);
  await page.waitForTimeout(1500);
  await dismiss(page);
  await page.locator('button:has-text("uisrc-key")').first().click();
  await page.waitForTimeout(600);
  await page.fill('input[placeholder^="git@github.com"]', 'git@192.0.2.5:o/r.git');
  await page.fill('input#branch, input[wire\\:model="branch"]', 'main');
  await page.screenshot({ path: `${out}${name}-form.png`, fullPage: true });
  await page.locator('button:has-text("Continue")').first().click();
}

async function editorForm(page, base, name, type, text, button) {
  await page.goto(`${base}/project/${PROJECT}/environment/${ENV}/new?type=${type}&destination=${dest}`);
  await page.waitForTimeout(2500);
  await dismiss(page);
  await page.locator('.monaco-editor').first().click();
  await page.keyboard.insertText(text);
  await page.screenshot({ path: `${out}${name}-form.png`, fullPage: true });
  await page.locator(`button:has-text("${button}")`).first().click();
}

// The Git Source page: moves an application to another provider's App.
async function changeSource(page, base, uuid, target, label) {
  await page.goto(`${base}/project/${PROJECT}/environment/${ENV}/application/${uuid}/source`);
  await page.waitForTimeout(2000);
  await dismiss(page);
  await page.screenshot({ path: `${out}pf-source-${label}.png`, fullPage: true });
  await page.locator(`button:has-text("${target}")`).locator('visible=true').first().click({ force: true });
  await page.waitForTimeout(600);
  await page.locator('input[placeholder*="Change Git Source"], input[type=text]').locator('visible=true').last().fill('Change Git Source');
  await page.waitForTimeout(400);
  await page.screenshot({ path: `${out}pf-source-${label}-confirm.png` });
  await page.locator('button:has-text("Continue")').locator('visible=true').last().click({ force: true });
  await page.waitForTimeout(2500);
  await page.screenshot({ path: `${out}pf-source-${label}-after.png`, fullPage: true });
}

const COLUMNS = `name, git_repository, git_branch, git_commit_sha, repository_project_id, source_type, build_pack, ports_exposes, publish_directory,
  base_directory, health_check_enabled, dockerfile, dockerfile_location, docker_compose_location, private_key_id, status`;
function row(uuid) {
  const t = JSON.parse(sql(`SELECT row_to_json(t) FROM (SELECT ${COLUMNS}, source_id, fqdn FROM applications WHERE uuid = '${uuid}') t`) || 'null');
  if (!t) return null;
  t.name = t.name?.replaceAll(uuid, '<uuid>').replace(/^[a-z]+-[a-z]+-.*$/, '<random>');
  t.fqdn = t.fqdn?.replaceAll(uuid, '<uuid>');
  t.settings = JSON.parse(sql(`SELECT row_to_json(s) FROM (SELECT is_static, is_git_shallow_clone_enabled, is_auto_deploy_enabled FROM application_settings WHERE application_id = (SELECT id FROM applications WHERE uuid = '${uuid}')) s`) || 'null');
  return t;
}

const cases = [
  ['github', (p, b, n) => repositoryForm(p, b, n, 'private-gh-app', 'uisrc-connected')],
  ['gitlab', (p, b, n) => repositoryForm(p, b, n, 'private-gitlab-app', 'uisrc-gl-on')],
  ['gitea', (p, b, n) => repositoryForm(p, b, n, 'private-gitea-app', 'uisrc-gt-on')],
  ['deploykey', deployKeyForm],
  ['dockerfile', (p, b, n) => editorForm(p, b, n, 'dockerfile', 'FROM nginx:alpine\nEXPOSE 8080\n', 'Create Application')],
];
const sides = [['ref', 'http://127.0.0.1:18000'], ['rs', `http://127.0.0.1:${rs}`]];
const pages = {};
for (const [label, base] of sides) pages[label] = await login(base);
let failed = 0;
for (const [name, run] of cases.filter(([n]) => !only.length || only.includes(n))) {
  const rows = {};
  for (const [label, base] of sides) {
    const page = pages[label];
    try {
      await run(page, base, `pf-${name}-${label}`);
      await page.waitForURL(/\/application\/[a-z0-9]+/, { timeout: 30000 });
      const uuid = page.url().match(/\/application\/([a-z0-9]+)/)[1];
      await page.waitForTimeout(1000);
      await page.screenshot({ path: `${out}pf-${name}-${label}-done.png`, fullPage: true });
      rows[label] = row(uuid);
      sql(`UPDATE applications SET deleted_at = NULL WHERE uuid = '${uuid}'`);
      const id = sql(`SELECT id FROM applications WHERE uuid = '${uuid}'`);
      sql(`DELETE FROM environment_variables WHERE resourceable_id = ${id} AND resourceable_type = 'App\\Models\\Application'`);
      sql(`DELETE FROM application_settings WHERE application_id = ${id}`);
      sql(`DELETE FROM applications WHERE id = ${id}`);
    } catch (e) {
      await page.screenshot({ path: `${out}pf-${name}-${label}-error.png`, fullPage: true }).catch(() => {});
      rows[label] = { error: String(e).slice(0, 300) };
    }
  }
  const same = !rows.ref?.error && !rows.rs?.error && JSON.stringify(rows.ref) === JSON.stringify(rows.rs);
  console.log(`${same ? 'PASS' : 'FAIL'} ${name}`);
  if (!same) {
    failed++;
    console.log('  ref', JSON.stringify(rows.ref));
    console.log('  rs ', JSON.stringify(rows.rs));
  }
}
if (!only.length || only.includes('source')) {
  const glId = sql("SELECT id FROM gitlab_apps WHERE name = 'uisrc-gl-on'");
  const rows = {};
  for (const [label, base] of sides) {
    const uuid = `gitsrc${label}source0000000`;
    const env = sql(`SELECT id FROM environments WHERE uuid = '${ENV}'`);
    const destId = sql(`SELECT id FROM standalone_dockers WHERE uuid = '${dest}'`);
    const id = sql(`INSERT INTO applications (uuid, name, git_repository, git_branch, build_pack, ports_exposes, environment_id, destination_id, destination_type, source_id, source_type, repository_project_id, created_at, updated_at) VALUES ('${uuid}', 'gitsrc-source', 'o/r', 'main', 'dockerfile', '80', ${env}, ${destId}, 'App\\Models\\StandaloneDocker', ${glId}, 'App\\Models\\GitlabApp', 77, now(), now()) RETURNING id`).split('\n')[0];
    sql(`INSERT INTO application_settings (application_id, created_at, updated_at) VALUES (${id}, now(), now())`);
    try {
      await changeSource(pages[label], base, uuid, 'uisrc-gt-on', label);
      rows[label] = JSON.parse(sql(`SELECT row_to_json(t) FROM (SELECT source_type, repository_project_id, a.git_repository FROM applications a WHERE id = ${id}) t`));
    } catch (e) {
      await pages[label].screenshot({ path: `${out}pf-source-${label}-error.png`, fullPage: true }).catch(() => {});
      rows[label] = { error: String(e).slice(0, 300) };
    }
    sql(`DELETE FROM application_settings WHERE application_id = ${id}`);
    sql(`DELETE FROM applications WHERE id = ${id}`);
  }
  const same = !rows.ref?.error && !rows.rs?.error && JSON.stringify(rows.ref) === JSON.stringify(rows.rs) && rows.rs.source_type?.includes('Gitea');
  console.log(`${same ? 'PASS' : 'FAIL'} source change`);
  if (!same) {
    failed++;
    console.log('  ref', JSON.stringify(rows.ref));
    console.log('  rs ', JSON.stringify(rows.rs));
  }
}
await browser.close();
process.exit(failed ? 1 : 0);
