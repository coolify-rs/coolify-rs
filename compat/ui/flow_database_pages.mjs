// Database pages on both UIs: General actions, Healthcheck, Backups, executions, Import. Each side gets its own
// PostgreSQL database on the test VM; toasts, rows, audit_events and VM state are diffed. Run:
//   node compat/ui/flow_database_pages.mjs <port> [part ...]   parts: general health backups import sqlite
// Everything created is tracked by uuid and deleted again, also after a failure.
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { flow, psql, auditMax, auditSince, tokens } from './flow.mjs';

const port = process.argv[2] ?? '18087';
const parts = process.argv.slice(3);
const on = (p) => !parts.length || parts.includes(p);
const SERVER = 'hml20fsj5eles0xfiqidq8j5';
const PROJECT = 'j8xm2emlllhfnyebjzgyzp7z';
const ENV = 'uj3st9yvtvzxh7rul3c3opcz';
const URL_BASE = `/project/${PROJECT}/environment/${ENV}/database`;
const vm = (cmd) => execFileSync('orb', ['-m', 'coolify-rs-e2e', '-u', 'root', 'bash', '-c', cmd], { encoding: 'utf8' }).trim();
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function api(method, p, body) {
  for (;;) {
    const r = await fetch(`http://127.0.0.1:18000/api/v1${p}`, {
      method,
      headers: { Authorization: `Bearer ${tokens.acme_owner_root}`, 'Content-Type': 'application/json', Accept: 'application/json' },
      body: body ? JSON.stringify(body) : undefined,
    });
    if (r.status === 429) {
      await sleep(5000);
      continue;
    }
    const t = await r.text();
    try {
      return { status: r.status, json: JSON.parse(t) };
    } catch {
      return { status: r.status, text: t };
    }
  }
}

const created = [];
async function createDb(kind, name, start = false) {
  const r = await api('POST', `/databases/${kind}`, { server_uuid: SERVER, project_uuid: PROJECT, environment_uuid: ENV, name, instant_deploy: false });
  if (r.status >= 300) throw new Error(`create ${kind}: ${r.status} ${JSON.stringify(r.json)}`);
  created.push(r.json.uuid);
  if (start) {
    await api('POST', `/databases/${r.json.uuid}/start`);
    for (let i = 0; i < 60; i++) {
      await sleep(3000);
      const s = await api('GET', `/databases/${r.json.uuid}`);
      if (String(s.json.status).startsWith('running')) break;
    }
  }
  return r.json.uuid;
}

async function cleanup() {
  for (const u of created.splice(0)) {
    await api('DELETE', `/databases/${u}?delete_volumes=true&delete_configurations=true&docker_cleanup=true`).catch(() => {});
  }
}

const table = { postgresql: 'standalone_postgresqls', sqlite: 'standalone_sqlites' };
const dbRow = (kind, uuid, cols) => JSON.parse(psql(`select row_to_json(r) from (select ${cols} from ${table[kind]} where uuid = '${uuid}') r`) || 'null');
const AUDIT = '^ui\\.(database|application\\.storage)';
const audit = (start, uuid, bk = []) =>
  auditSince(start, AUDIT).filter((r) => JSON.stringify(r).includes(uuid)).map((r) =>
    JSON.parse(
      [uuid, ...bk].reduce((t, u) => t.replaceAll(u, 'UUID'), JSON.stringify(r)).replace(/pages-[a-z]+-(ref|rs)/g, 'NAME'),
    ),
  );
const saveBar = async (h) => {
  await h.page.getByRole('button', { name: /^Save/ }).first().click();
};
const passwordStep = async (h, typed) => {
  // ConfirmModal: optional checkbox step, the confirm text, the password.
  const p = h.page;
  for (let i = 0; i < 5; i++) {
    // Only the modal's own field; a failure must not print the value.
    const pw = p.locator(':is(.fixed, [role=dialog]) input[type=password]:visible:not([readonly])');
    if (await pw.count()) {
      await pw.last().fill(h.secret, { timeout: 3000 }).catch(() => {
        throw new Error('The password field could not be filled.');
      });
      await p.getByRole('button', { name: /^Confirm$/ }).last().click();
      return;
    }
    const button = p.getByRole('button', { name: /^(Continue|Confirm|Permanently delete|Enable healthcheck)$/ }).last();
    if (!(await button.isVisible().catch(() => false))) return;
    if (!(await button.isEnabled())) {
      await p.locator(':is(.fixed, [role=dialog]) input:visible:not([type=checkbox]):not([type=password])').last().fill(typed ?? '', { timeout: 2000 });
      await p.waitForTimeout(150);
    }
    await button.click({ timeout: 3000 });
    await p.waitForTimeout(350);
  }
};

try {
  if (on('general')) {
    await flow(port, 'alice', async (h) => {
      const uuid = await createDb('postgresql', `pages-pg-${h.label}`, true);
      const out = {};
      await h.go(`${URL_BASE}/${uuid}`);
      await h.shot('dbf-general-running');
      const start = auditMax();
      // Validation of the form.
      await h.field('Name').fill('bad<name>$');
      await saveBar(h);
      out.bad_name = await h.toasts();
      out.bad_name_inline = await h.page.locator('.text-red-500:visible').allInnerTexts();
      await h.go(`${URL_BASE}/${uuid}`);
      // Public port, then Public access.
      await h.field('Public port').fill('15987');
      await saveBar(h);
      out.port_saved = await h.toasts(1500);
      await h.listbox('Access', 'Public through TCP proxy');
      out.public_toast = await h.toasts(9000);
      console.log(h.label, 'port', out.port_saved, 'public', out.public_toast);
      out.public_row = dbRow('postgresql', uuid, 'is_public, public_port, public_port_timeout');
      out.proxy = vm(`docker ps --filter name=${uuid}-proxy --format '{{.Image}} {{.Ports}}'`).replace(/\s+/g, ' ');
      await h.go(`${URL_BASE}/${uuid}`);
      await h.shot('dbf-general-public');
      await h.button('View logs').click();
      await h.page.waitForTimeout(4000);
      await h.shot('dbf-proxy-logs', false);
      await h.page.keyboard.press('Escape');
      await h.page.locator('[aria-label="Close"]:visible').first().click({ timeout: 500 }).catch(() => {});
      await h.go(`${URL_BASE}/${uuid}`);
      await h.listbox('Access', 'Private');
      out.private_toast = await h.toasts(7000);
      out.private_row = dbRow('postgresql', uuid, 'is_public, public_port');
      out.proxy_gone = vm(`docker ps -a --filter name=${uuid}-proxy --format '{{.Names}}' | wc -l`);
      // Log drain on a server without one.
      await h.go(`${URL_BASE}/${uuid}`);
      await h.listbox('Log drain', 'Forward logs');
      out.logdrain = await h.toasts();
      // Init scripts.
      await h.go(`${URL_BASE}/${uuid}`);
      await h.button('+ Add').click();
      await h.page.waitForTimeout(300);
      await h.page.fill('input[placeholder="create_test_db.sql"]:visible', 'a b.sql');
      await h.page.fill('textarea[placeholder="CREATE DATABASE test;"]:visible', 'select 1;');
      await h.page.getByRole('button', { name: 'Add script' }).click();
      out.script_bad = await h.toasts();
      await h.page.fill('input[placeholder="create_test_db.sql"]:visible', 'one.sql');
      await h.page.getByRole('button', { name: 'Add script' }).click();
      out.script_add = await h.toasts();
      out.scripts = dbRow('postgresql', uuid, 'init_scripts');
      await h.go(`${URL_BASE}/${uuid}`);
      await h.shot('dbf-general-scripts');
      await h.page.getByRole('button', { name: 'Delete', exact: true }).first().click();
      await h.page.waitForTimeout(300);
      await h.shot('dbf-script-delete', false);
      await passwordStep(h, 'one.sql');
      out.script_delete = await h.toasts(2500);
      out.scripts_after = dbRow('postgresql', uuid, 'init_scripts');
      // SSL while stopped.
      await api('POST', `/databases/${uuid}/stop`);
      for (let i = 0; i < 20; i++) {
        await sleep(2000);
        if (String((await api('GET', `/databases/${uuid}`)).json.status).startsWith('exited')) break;
      }
      await h.go(`${URL_BASE}/${uuid}`);
      await h.shot('dbf-general-exited');
      out.ssl_row_before = dbRow('postgresql', uuid, 'enable_ssl, ssl_mode');
      await h.listbox('SSL', 'Enabled');
      out.ssl_toast = await h.toasts();
      await h.go(`${URL_BASE}/${uuid}`);
      await h.listbox('SSL mode', 'verify-ca');
      out.ssl_toast2 = await h.toasts();
      out.ssl_row = dbRow('postgresql', uuid, 'enable_ssl, ssl_mode');
      out.audit = audit(start, uuid);
      return out;
    }, { after: () => {} });
    await cleanup();
  }

  if (on('health')) {
    await flow(port, 'alice', async (h) => {
      const uuid = await createDb('postgresql', `pages-pg-${h.label}`);
      const out = {};
      const row = () => dbRow('postgresql', uuid, 'health_check_enabled, health_check_interval, health_check_timeout, health_check_retries, health_check_start_period');
      await h.go(`${URL_BASE}/${uuid}/healthcheck`);
      await h.shot('dbf-health');
      out.bad = null;
      await h.field('Interval').fill('0');
      await saveBar(h);
      out.bad_toast = await h.toasts();
      out.bad_error = await h.page.locator('.text-red-500:visible').allInnerTexts();
      await h.field('Interval').fill('30');
      await h.field('Retries').fill('7');
      await saveBar(h);
      out.save = await h.toasts();
      out.row = row();
      await h.button('Disable healthcheck').click();
      out.disable = await h.toasts();
      out.row_off = row();
      await h.go(`${URL_BASE}/${uuid}/healthcheck`);
      await h.shot('dbf-health-off');
      await h.button('Enable healthcheck').click();
      await h.page.waitForTimeout(300);
      await h.shot('dbf-health-confirm', false);
      await h.page.getByRole('button', { name: 'Enable healthcheck' }).last().click();
      out.enable = await h.toasts(2500);
      out.row_on = row();
      return out;
    });
    await cleanup();
  }

  if (on('backups')) {
    await flow(port, 'alice', async (h) => {
      const uuid = await createDb('postgresql', `pages-pg-${h.label}`, true);
      const out = {};
      const start = auditMax();
      const BK_COLS = `enabled, frequency, save_s3, s3_storage_id, disable_local_backup, databases_to_backup, dump_all, timeout, missing_backup_notification_days,
        database_backup_retention_amount_locally, database_backup_retention_days_locally, database_backup_retention_max_storage_locally,
        database_backup_retention_amount_s3, database_backup_retention_days_s3, database_backup_retention_max_storage_s3, database_type, team_id`;
      const row = (u) => JSON.parse(psql(`select row_to_json(r) from (select ${BK_COLS} from scheduled_database_backups where uuid = '${u}') r`) || 'null');
      await h.go(`${URL_BASE}/${uuid}/backups`);
      await h.button('+ Add').click();
      await h.page.waitForTimeout(300);
      await h.shot('dbf-bk-new', false);
      await h.page.fill('input[placeholder="0 0 * * * or daily"]:visible', 'not a cron');
      await h.page.getByRole('button', { name: 'Add schedule' }).click();
      out.bad_frequency = await h.toasts();
      await h.go(`${URL_BASE}/${uuid}/backups`);
      await h.button('+ Add').click();
      await h.page.waitForTimeout(300);
      await h.page.fill('input[placeholder="0 0 * * * or daily"]:visible', 'daily');
      await h.page.getByRole('button', { name: 'Add schedule' }).click();
      await h.page.waitForURL(/\/backups\/[a-z0-9]+$/, { timeout: 10000 });
      const bk = h.page.url().split('/').pop();
      out.created_toast = await h.toasts();
      out.row_created = row(bk);
      await h.go(`${URL_BASE}/${uuid}/backups/${bk}`);
      await h.shot('dbf-bk-general');
      await h.go(`${URL_BASE}/${uuid}/backups/${bk}/s3`);
      out.s3_redirect = new URL(h.page.url()).pathname.endsWith(`/backups/${bk}`);
      await h.go(`${URL_BASE}/${uuid}/backups`);
      await h.shot('dbf-bk-index');
      await h.go(`${URL_BASE}/${uuid}/backups/${bk}`);
      // Edits.
      await h.field('Timeout').fill('10');
      await saveBar(h);
      out.bad_timeout = await h.toasts();
      await h.field('Timeout').fill('7200');
      await h.field('Frequency').fill('0 3 * * *');
      await h.field('Missing backup alert after').fill('2');
      await saveBar(h);
      out.save = await h.toasts();
      out.row_saved = row(bk);
      await h.go(`${URL_BASE}/${uuid}/backups/${bk}`);
      await h.listbox('Database selection', 'All databases');
      out.dump_all = await h.toasts();
      out.row_dump = row(bk);
      await h.go(`${URL_BASE}/${uuid}/backups/${bk}`);
      await h.shot('dbf-bk-dumpall');
      await h.listbox('Database selection', 'Specific databases');
      await h.toasts();
      // Enable / disable in the header.
      await h.go(`${URL_BASE}/${uuid}/backups/${bk}`);
      await h.page.locator('#resource-action-hud-slot').getByRole('button', { name: 'Disable backup' }).click();
      out.disabled = await h.toasts();
      out.row_off = row(bk);
      await h.shot('dbf-bk-disabled', false);
      await h.page.locator('#resource-action-hud-slot').getByRole('button', { name: 'Enable backup' }).click();
      out.enabled = await h.toasts();
      // Empty executions.
      await h.go(`${URL_BASE}/${uuid}/backups/${bk}/executions`);
      await h.shot('dbf-bk-exec-empty');
      // Back up now.
      await h.go(`${URL_BASE}/${uuid}/backups/${bk}`);
      await h.page.locator('#resource-action-hud-slot').getByRole('button', { name: 'Back up now' }).click();
      await h.page.waitForURL(/\/executions$/, { timeout: 15000 });
      out.backup_toast = await h.toasts();
      const execRow = () =>
        JSON.parse(psql(`select row_to_json(r) from (select e.id, e.status, e.filename, e.size, e.database_name, e.local_storage_deleted, e.s3_uploaded, e.message from scheduled_database_backup_executions e join scheduled_database_backups b on b.id = e.scheduled_database_backup_id where b.uuid = '${bk}' order by e.id desc limit 1) r`) || 'null');
      let ex = null;
      for (let i = 0; i < 40; i++) {
        await sleep(3000);
        ex = execRow();
        if (ex && ex.status !== 'running') break;
      }
      out.execution = { ...ex, id: 'ID', filename: ex?.filename?.replace(uuid, 'UUID').replace(/pg-dump-.*/, 'FILE'), size: ex?.size ? 'SIZE' : null };
      await h.go(`${URL_BASE}/${uuid}/backups/${bk}/executions`);
      await h.shot('dbf-bk-exec');
      // File on the VM and its download.
      const sum = vm(`sha256sum '${ex.filename}' | cut -d' ' -f1`);
      out.file_on_vm = sum.length === 64;
      const dl = await h.page.request.get(`${h.base}/download/backup/${ex.id}`, { failOnStatusCode: false });
      const buf = await dl.body();
      const crypto = await import('node:crypto');
      out.download_ok = crypto.createHash('sha256').update(buf).digest('hex') === sum;
      out.download_headers = [dl.status(), dl.headers()['content-type'], (dl.headers()['content-disposition'] ?? '').replace(/pg-dump[^"]*/, 'FILE')];
      const nf = await h.page.request.get(`${h.base}/download/backup/999999999`, { failOnStatusCode: false });
      out.download_missing = nf.status();
      // Restore that backup from a server path into the same database.
      await h.page.waitForTimeout(500);
      // Clean failed: nothing to do; Clean deleted entries: nothing to do.
      await h.button('Clean failed backups').click();
      out.clean_failed = await h.toasts();
      await h.button('Clean deleted entries').click();
      await h.page.waitForTimeout(300);
      await h.shot('dbf-bk-clean-confirm', false);
      await passwordStep(h, 'cleanup deleted backups');
      out.clean_deleted = await h.toasts(2500);
      // Delete the execution.
      await h.go(`${URL_BASE}/${uuid}/backups/${bk}/executions`);
      await h.page.locator('[aria-label="Delete backup"]').first().click();
      await h.page.waitForTimeout(300);
      await h.shot('dbf-bk-exec-del', false);
      await passwordStep(h, ex.filename);
      out.delete_exec = await h.toasts(3000);
      out.file_gone = vm(`test -e '${ex.filename}' && echo present || echo gone`);
      out.executions_left = psql(`select count(*) from scheduled_database_backup_executions e join scheduled_database_backups b on b.id = e.scheduled_database_backup_id where b.uuid = '${bk}'`);
      // Danger.
      await h.go(`${URL_BASE}/${uuid}/backups/${bk}/danger`);
      await h.shot('dbf-bk-danger');
      await h.button('Delete schedule').click();
      await h.page.waitForTimeout(300);
      await h.shot('dbf-bk-danger-confirm', false);
      await passwordStep(h, `pages-pg-${h.label}`);
      await h.page.waitForURL(/\/backups$/, { timeout: 8000 }).catch(() => {});
      out.after_delete = new URL(h.page.url()).pathname.replace(uuid, 'UUID');
      out.row_deleted = row(bk);
      out.audit = audit(start, uuid, [bk]);
      return out;
    });
    await cleanup();
  }

  if (on('import')) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'dbf-'));
    const sqlFile = path.join(dir, 'restore-me.sql');
    fs.writeFileSync(sqlFile, 'CREATE TABLE IF NOT EXISTS restored_marker (id int); INSERT INTO restored_marker VALUES (42);\n');
    await flow(port, 'alice', async (h) => {
      const uuid = await createDb('postgresql', `pages-pg-${h.label}`);
      const out = {};
      const start = auditMax();
      await h.go(`${URL_BASE}/${uuid}/import-backup`);
      await h.shot('dbf-imp-exited');
      await api('POST', `/databases/${uuid}/start`);
      for (let i = 0; i < 60; i++) {
        await sleep(3000);
        if (String((await api('GET', `/databases/${uuid}`)).json.status).startsWith('running')) break;
      }
      await h.go(`${URL_BASE}/${uuid}/import-backup`);
      await h.shot('dbf-imp');
      const cmd = () => h.page.locator('textarea:visible').first().inputValue();
      out.command_one = await cmd();
      await h.listbox('Backup contents', 'all databases');
      await h.page.waitForTimeout(1200);
      out.command_all = await cmd();
      await h.shot('dbf-imp-all');
      await h.listbox('Backup contents', 'one database');
      await h.page.waitForTimeout(800);
      await h.page.getByRole('button', { name: /^File/ }).first().dispatchEvent('click');
      await h.page.waitForTimeout(300);
      await h.shot('dbf-imp-file');
      // Server path: wrong, unsafe, then real.
      const location = h.page.locator('input[placeholder="/home/user/backup.sql.gz"]:visible');
      await location.fill('relative/path');
      await h.page.getByRole('button', { name: 'Check file' }).first().dispatchEvent('click');
      out.path_invalid = await h.toasts();
      await location.fill('/tmp/does-not-exist.sql');
      await h.page.getByRole('button', { name: 'Check file' }).first().dispatchEvent('click');
      out.path_missing = await h.toasts(2500);
      vm(`echo 'CREATE TABLE IF NOT EXISTS from_path (id int);' > /tmp/dbf-${h.label}.sql`);
      await location.fill(`/tmp/dbf-${h.label}.sql`);
      await h.page.getByRole('button', { name: 'Check file' }).first().dispatchEvent('click');
      out.path_ok = await h.toasts(2500);
      await h.shot('dbf-imp-path-ok');
      await h.page.getByRole('button', { name: 'Restore from file' }).dispatchEvent('click');
      await h.page.waitForTimeout(300);
      await h.shot('dbf-imp-confirm', false);
      await passwordStep(h, 'Confirm Deletion');
      await h.page.waitForTimeout(9000);
      await h.shot('dbf-imp-monitor', false);
      await h.page.reload();
      await h.page.waitForTimeout(800);
      out.tables_path = vm(`docker exec ${uuid} psql -U postgres -d postgres -Atc "select count(*) from information_schema.tables where table_name='from_path'"`);
      vm(`rm -f /tmp/dbf-${h.label}.sql`);
      // Upload.
      await h.go(`${URL_BASE}/${uuid}/import-backup`);
      await h.page.getByRole('button', { name: /^File/ }).first().dispatchEvent('click');
      await h.page.waitForTimeout(300);
      const input = h.page.locator('input[type=file]');
      await input.setInputFiles(sqlFile);
      await h.page.waitForTimeout(3000);
      await h.shot('dbf-imp-uploaded');
      await h.page.getByRole('button', { name: 'Restore from file' }).dispatchEvent('click');
      await h.page.waitForTimeout(300);
      await passwordStep(h, 'Confirm Deletion');
      await h.page.waitForTimeout(9000);
      out.tables_upload = vm(`docker exec ${uuid} psql -U postgres -d postgres -Atc "select id from restored_marker"`);
      out.audit = audit(start, uuid);
      return out;
    });
    fs.rmSync(dir, { recursive: true, force: true });
    await cleanup();
  }

  // Real backups and restores of the other engines: backup through the page, restore that file from its server path.
  for (const [kind, table, pick] of [
    ['mysql', 'standalone_mysqls', (u) => `docker exec ${u} sh -c 'mysql -uroot -p"$MYSQL_ROOT_PASSWORD" -e "CREATE TABLE IF NOT EXISTS default.marker (id int); DROP TABLE default.marker; " 2>&1 | tail -1' || true`],
    ['mariadb', 'standalone_mariadbs', () => 'true'],
    ['mongodb', 'standalone_mongodbs', () => 'true'],
    ['sqlite', 'standalone_sqlites', () => 'true'],
  ]) {
    void table;
    void pick;
    if (!on(`engine-${kind}`)) continue;
    await flow(port, 'alice', async (h) => {
      const uuid = await createDb(kind, `pages-${kind}-${h.label}`, true);
      const out = {};
      const norm = (t) => (t ?? '').replaceAll(uuid, 'UUID');
      await h.go(`${URL_BASE}/${uuid}`);
      await h.shot(`dbf-${kind}-general`);
      await h.go(`${URL_BASE}/${uuid}/import-backup`);
      await h.shot(`dbf-${kind}-import`);
      out.command = norm(await h.page.locator('textarea:visible').first().inputValue());
      if (kind !== 'sqlite') {
        await h.listbox('Backup contents', 'all databases');
        await h.page.waitForTimeout(1200);
        out.command_all = norm(await h.page.locator('textarea:visible').first().inputValue());
        if (kind !== 'mongodb') {
          await h.shot(`dbf-${kind}-import-all`);
        }
        await h.listbox('Backup contents', 'one database');
        await h.page.waitForTimeout(800);
      }
      await h.go(`${URL_BASE}/${uuid}/backups`);
      await h.button('+ Add').click();
      await h.page.waitForTimeout(300);
      await h.page.fill('input[placeholder="0 0 * * * or daily"]:visible', '0 4 * * *');
      await h.page.getByRole('button', { name: 'Add schedule' }).click();
      await h.page.waitForURL(/\/backups\/[a-z0-9]+$/, { timeout: 10000 });
      const bk = h.page.url().split('/').pop();
      await h.go(`${URL_BASE}/${uuid}/backups/${bk}`);
      await h.shot(`dbf-${kind}-bk`);
      out.row = JSON.parse(psql(`select row_to_json(r) from (select databases_to_backup, dump_all, frequency, enabled from scheduled_database_backups where uuid = '${bk}') r`));
      await h.page.locator('#resource-action-hud-slot').getByRole('button', { name: 'Back up now' }).dispatchEvent('click');
      await h.page.waitForURL(/\/executions$/, { timeout: 15000 });
      let ex = null;
      for (let i = 0; i < 50; i++) {
        await sleep(3000);
        ex = JSON.parse(psql(`select row_to_json(r) from (select e.id, e.status, e.filename, e.size, e.database_name, e.message from scheduled_database_backup_executions e join scheduled_database_backups b on b.id = e.scheduled_database_backup_id where b.uuid = '${bk}' order by e.id desc limit 1) r`) || 'null');
        if (ex && ex.status !== 'running') break;
      }
      out.execution = { status: ex?.status, database_name: ex?.database_name, has_size: Number(ex?.size) > 0, message: norm(ex?.message), file: norm(ex?.filename).replace(/[^/]*$/, '').replace(/pages-[a-z]+-(ref|rs)-/, 'NAME-'), name: (ex?.filename ?? '').split('/').pop().replace(/[0-9]{10,}.*/, 'TIME') };
      await h.go(`${URL_BASE}/${uuid}/backups/${bk}/executions`);
      await h.shot(`dbf-${kind}-bk-exec`);
      // Restore the same file.
      await h.go(`${URL_BASE}/${uuid}/import-backup`);
      await h.page.getByRole('button', { name: /^File/ }).first().dispatchEvent('click');
      await h.page.waitForTimeout(300);
      await h.page.locator('input[placeholder="/home/user/backup.sql.gz"]:visible').fill(ex.filename);
      await h.page.getByRole('button', { name: 'Check file' }).first().dispatchEvent('click');
      out.check = await h.toasts(2500);
      await h.page.getByRole('button', { name: 'Restore from file' }).dispatchEvent('click');
      await h.page.waitForTimeout(300);
      await passwordStep(h, 'Confirm Deletion');
      let act = null;
      for (let i = 0; i < 40; i++) {
        await sleep(3000);
        act = JSON.parse(psql(`select row_to_json(r) from (select properties->>'status' as status, properties->>'exitCode' as exit_code from activity_log where properties->>'type_uuid' is not null and properties::text like '%${uuid}%' and created_at > now() - interval '5 minutes' order by id desc limit 1) r`) || 'null');
        if (act && !['queued', 'in_progress'].includes(act.status)) break;
      }
      out.restore = act;
      console.log(h.label, kind, 'restore', JSON.stringify(act));
      await h.shot(`dbf-${kind}-restore`, false);
      return out;
    });
    await cleanup();
  }

  if (on('ssl')) {
    await flow(port, 'alice', async (h) => {
      const uuid = await createDb('postgresql', `pages-pg-${h.label}`);
      const out = {};
      psql(`update standalone_postgresqls set enable_ssl = true where uuid = '${uuid}'`);
      await api('POST', `/databases/${uuid}/start`);
      for (let i = 0; i < 60; i++) {
        await sleep(3000);
        if (String((await api('GET', `/databases/${uuid}`)).json.status).startsWith('running')) break;
      }
      const cert = () => psql(`select count(*), max(md5(ssl_certificate)) from ssl_certificates where resource_type = 'App\\Models\\StandalonePostgresql' and resource_id = (select id from standalone_postgresqls where uuid = '${uuid}')`);
      const before = cert();
      await h.go(`${URL_BASE}/${uuid}`);
      await h.shot('dbf-ssl-on');
      out.mount = vm(`ls /data/coolify/databases/${uuid}/ssl 2>/dev/null | sort | tr '\\n' ' '`);
      await h.button('Regenerate SSL Certificates').click();
      await h.page.waitForTimeout(300);
      await h.shot('dbf-ssl-confirm', false);
      await passwordStep(h, null);
      out.toast = await h.toasts(4000);
      const after = cert();
      out.changed = before !== after;
      out.count = after.split('|')[0];
      out.mount_after = vm(`ls /data/coolify/databases/${uuid}/ssl 2>/dev/null | sort | tr '\\n' ' '`);
      return out;
    });
    await cleanup();
  }

  if (on('s3')) {
    const NAME = 'coolify-rs-pages-minio';
    const MINIO = '19087';
    const ip = execFileSync('ipconfig', ['getifaddr', 'en0'], { encoding: 'utf8' }).trim();
    const docker = (...a) => execFileSync('docker', a, { encoding: 'utf8' }).trim();
    const php = (code) => docker('exec', 'coolify-rs-reference-coolify-1', 'php', '/var/www/html/artisan', 'tinker', '--execute', code);
    docker('rm', '-f', NAME);
    docker('run', '-d', '--name', NAME, '-p', `0.0.0.0:${MINIO}:9000`, '-e', 'MINIO_ROOT_USER=pageskey', '-e', 'MINIO_ROOT_PASSWORD=pagessecret', 'minio/minio', 'server', '/data');
    await sleep(4000);
    docker('exec', NAME, 'mkdir', '-p', '/data/pagesbucket');
    let storage = '';
    try {
      storage = php(`echo App\\Models\\S3Storage::forceCreate(['name'=>'pages-s3','region'=>'us-east-1','key'=>'pageskey','secret'=>'pagessecret','bucket'=>'pagesbucket','endpoint'=>'http://${ip}:${MINIO}','is_usable'=>true,'team_id'=>4])->uuid;`).split('\n').pop();
      await flow(port, 'alice', async (h) => {
        const uuid = await createDb('postgresql', `pages-pg-${h.label}`, true);
        const out = {};
        psql(`update s3_storages set is_usable = true where uuid = '${storage}'`);
        const bkc = await api('POST', `/databases/${uuid}/backups`, { frequency: 'daily', enabled: true });
        const bk = bkc.json.uuid;
        console.log(h.label, 'storage', storage, psql(`select count(*), bool_or(is_usable), max(team_id) from s3_storages where uuid = '${storage}'`), 'backup', bk ?? JSON.stringify(bkc));
        const row = () => JSON.parse(psql(`select row_to_json(r) from (select save_s3, disable_local_backup, s3_storage_id is not null as primary_set, (select count(*) from scheduled_database_backup_s3_storage p where p.scheduled_database_backup_id = b.id) as storages from scheduled_database_backups b where uuid = '${bk}') r`));
        await h.go(`${URL_BASE}/${uuid}/backups/${bk}`);
        await h.shot('dbf-s3-on-default');
        await h.button('Disable S3').click();
        out.disable = await h.toasts(2000);
        out.row_off = row();
        await h.go(`${URL_BASE}/${uuid}/backups/${bk}`);
        await h.shot('dbf-s3-off');
        await h.button('Enable S3').click();
        out.enable = await h.toasts(2000);
        out.row_on = row();
        await h.go(`${URL_BASE}/${uuid}/backups/${bk}`);
        await h.shot('dbf-s3-on');
        await h.go(`${URL_BASE}/${uuid}/backups`);
        await h.shot('dbf-s3-index');
        await h.go(`${URL_BASE}/${uuid}/backups/${bk}`);
        await h.page.locator('#resource-action-hud-slot').getByRole('button', { name: 'Back up now' }).dispatchEvent('click');
        await h.page.waitForURL(/\/executions$/, { timeout: 15000 });
        let ex = null;
        for (let i = 0; i < 40; i++) {
          await sleep(3000);
          ex = JSON.parse(psql(`select row_to_json(r) from (select e.id, e.status, e.filename, e.s3_uploaded, e.s3_storage_deleted, e.local_storage_deleted, e.message, (select json_agg(json_build_object('uploaded', x.s3_uploaded, 'deleted', x.s3_storage_deleted, 'message', x.message)) from database_backup_s3_replicas x where x.execution_id = e.id) as replicas from scheduled_database_backup_executions e join scheduled_database_backups b on b.id = e.scheduled_database_backup_id where b.uuid = '${bk}' order by e.id desc limit 1) r`) || 'null');
          if (ex && ex.status !== 'running') break;
        }
        await sleep(2000);
        out.execution = JSON.parse(JSON.stringify({ ...ex, id: 'ID', filename: 'F', message: (ex?.message ?? '').replaceAll(uuid, 'UUID') }).replaceAll(ex.filename, 'F'));
        out.object = docker('exec', NAME, 'sh', '-c', `test -e '/data/pagesbucket${ex.filename}' && echo present || echo missing`);
        await h.go(`${URL_BASE}/${uuid}/backups/${bk}/executions`);
        await h.shot('dbf-s3-exec');
        out.usable_after_backup = psql(`select is_usable from s3_storages where uuid = '${storage}'`);
        psql(`update s3_storages set is_usable = true where uuid = '${storage}'`);
        // Import from S3: the check goes through testConnection.
        await h.go(`${URL_BASE}/${uuid}/import-backup`);
        await h.page.getByRole('button', { name: /^S3 storage/ }).first().dispatchEvent('click');
        await h.page.waitForTimeout(300);
        await h.shot('dbf-s3-import');
        await h.listbox('S3 storage', 'pages-s3');
        const path = h.page.locator('input[placeholder="/backups/database-backup.gz"]:visible');
        await path.fill(ex.filename);
        await path.press('Tab');
        await h.page.waitForTimeout(600);
        await h.page.getByRole('button', { name: 'Check file' }).last().dispatchEvent('click');
        out.check_s3 = await h.toasts(4000);
        await h.shot('dbf-s3-import-checked', false);
        out.usable_after = psql(`select is_usable from s3_storages where uuid = '${storage}'`);
        // Delete the execution from S3 as well.
        psql(`update s3_storages set is_usable = true where uuid = '${storage}'`);
        await h.go(`${URL_BASE}/${uuid}/backups/${bk}/executions`);
        await h.page.locator('[aria-label="Delete backup"]').first().dispatchEvent('click');
        await h.page.waitForTimeout(300);
        await h.shot('dbf-s3-exec-del', false);
        const box = h.page.locator(':is(.fixed, [role=dialog]) input[type=checkbox]:visible').first();
        if (await box.count()) await box.check({ force: true });
        await passwordStep(h, ex.filename);
        out.delete_exec = (await h.toasts(4000)).map((t) => t.replaceAll(ex.filename, 'F'));
        out.object_after = docker('exec', NAME, 'sh', '-c', `test -e '/data/pagesbucket${ex.filename}' && echo present || echo missing`);
        await api('DELETE', `/databases/${uuid}/backups/${bk}`);
        return out;
      });
    } finally {
      await cleanup();
      if (storage) {
        psql(`delete from scheduled_database_backup_s3_storage where s3_storage_id in (select id from s3_storages where uuid = '${storage}')`);
        psql(`delete from s3_storages where uuid = '${storage}'`);
      }
      docker('rm', '-f', NAME);
    }
  }

  if (on('volume-download')) {
    await flow(port, 'alice', async (h) => {
      const uuid = await createDb('postgresql', `pages-pg-${h.label}`, true);
      const out = {};
      const storage = (await api('GET', `/databases/${uuid}/storages`)).json.persistent_storages[0].uuid;
      const put = await api('PUT', `/databases/${uuid}/storages/${storage}/backups`, { frequency: 'daily' });
      await api('POST', `/databases/${uuid}/storages/${storage}/backups/run`);
      let ex = null;
      for (let i = 0; i < 40; i++) {
        await sleep(3000);
        ex = JSON.parse(psql(`select row_to_json(r) from (select e.id, e.status, e.filename from scheduled_volume_backup_executions e join scheduled_volume_backups b on b.id = e.scheduled_volume_backup_id where b.uuid = '${put.json.uuid}' order by e.id desc limit 1) r`) || 'null');
        if (ex && ex.status !== 'running') break;
      }
      const sum = vm(`sha256sum '${ex.filename}' | cut -d' ' -f1`);
      const dl = await h.page.request.get(`${h.base}/download/volume-backup/${ex.id}`, { failOnStatusCode: false });
      const crypto = await import('node:crypto');
      const bytes = await dl.body();
      out.same_bytes = crypto.createHash('sha256').update(bytes).digest('hex') === sum;
      console.log(h.label, 'download', bytes.length, 'file', vm(`stat -c %s '${ex.filename}'`), bytes.subarray(0, 4).toString('hex'));
      out.headers = [dl.status(), dl.headers()['content-type'], (dl.headers()['content-disposition'] ?? '').replace(/volume-[^"]*/, 'FILE')];
      const nf = await h.page.request.get(`${h.base}/download/volume-backup/999999999`, { failOnStatusCode: false });
      out.missing = [nf.status(), await nf.text()];
      await api('DELETE', `/databases/${uuid}/storages/${storage}/backups`);
      return out;
    });
    await cleanup();
  }

  if (on('sqlite-connect')) {
    await flow(port, 'alice', async (h) => {
      const uuid = await createDb('sqlite', `pages-sqlite-${h.label}`);
      const out = {};
      const app = await api('POST', '/applications/dockerimage', { project_uuid: PROJECT, environment_uuid: ENV, server_uuid: SERVER, docker_registry_image_name: 'nginx', docker_registry_image_tag: 'alpine', ports_exposes: '80', name: `pages-app-${h.label}`, instant_deploy: false });
      const compose = await api('POST', '/applications/dockercompose', { project_uuid: PROJECT, environment_uuid: ENV, server_uuid: SERVER, name: `pages-compose-${h.label}`, instant_deploy: false, docker_compose_raw: Buffer.from('services:\n  web:\n    image: nginx:alpine\n').toString('base64') });
      try {
        const norm = (t) => t.replaceAll(uuid, 'DB').replaceAll(app.json.uuid, 'APP').replace(/-(ref|rs)\b/g, '-SIDE');
        await h.go(`${URL_BASE}/${uuid}`);
        await h.shot('dbf-sqlite-connect');
        const pick = async (name) => {
          await h.page.locator('#applicationUuid, button:visible', { hasText: /Select an application|pages-/ }).first().click();
          await h.page.waitForTimeout(250);
          await h.page.locator('.listbox-option:visible', { hasText: name }).first().click();
          await h.page.waitForTimeout(250);
        };
        await pick(`pages-compose-${h.label}`);
        await h.field('Mount path').fill('bad path');
        await h.button('Mount volume').click();
        out.bad_path = (await h.toasts(1500)).concat(await h.page.locator('.text-red-500:visible').allInnerTexts());
        await h.field('Mount path').fill('/srv/db');
        await h.button('Mount volume').click();
        await h.page.waitForTimeout(1500);
        await h.shot('dbf-sqlite-compose');
        out.snippet = norm(await h.page.locator('pre:visible').first().innerText());
        await pick(`pages-app-${h.label}`);
        await h.button('Mount volume').click();
        await h.page.waitForURL(/persistent-storage$/, { timeout: 8000 }).catch(() => {});
        out.redirect = norm(new URL(h.page.url()).pathname);
        const row = () => JSON.parse(psql(`select coalesce(json_agg(r), '[]') from (select v.name, v.mount_path, v.host_path, v.resource_type, v.is_preview_suffix_enabled, v.uuid is not null as has_uuid from local_persistent_volumes v where v.standalone_sqlite_id = (select id from standalone_sqlites where uuid = '${uuid}')) r`));
        out.rows = JSON.parse(norm(JSON.stringify(row())));
        await h.go(`${URL_BASE}/${uuid}`);
        await h.shot('dbf-sqlite-linked');
        out.again_toast = null;
        await pick(`pages-app-${h.label}`);
        await h.button('Mount volume').click();
        out.again_toast = (await h.toasts(1500)).map(norm);
        await h.go(`${URL_BASE}/${uuid}`);
        await h.button('Unlink').click();
        out.unlink = await h.toasts(1500);
        out.rows_after = row();
      } finally {
        for (const a of [app, compose]) {
          if (a.json?.uuid) await api('DELETE', `/applications/${a.json.uuid}?delete_volumes=true&delete_configurations=true&docker_cleanup=false`);
        }
      }
      return out;
    });
    await cleanup();
  }

  if (on('member')) {
    const uuid = await createDb('postgresql', 'pages-member');
    const sq = await createDb('sqlite', 'pages-member-sq');
    const bk = (await api('POST', `/databases/${uuid}/backups`, { frequency: 'daily', enabled: true })).json.uuid;
    await flow(port, 'bob', async (h) => {
      const out = {};
      for (const [n, p] of [['general', uuid], ['health', `${uuid}/healthcheck`], ['import', `${uuid}/import-backup`], ['backups', `${uuid}/backups`], ['sqlite', sq], ['backup', `${uuid}/backups/${bk}`], ['backup-exec', `${uuid}/backups/${bk}/executions`]]) {
        await h.go(`${URL_BASE}/${p}`);
        await h.shot(`dbf-member-${n}`);
        out[n] = (await h.page.locator('button:visible').allInnerTexts()).filter((t) => /Add|Enable|Disable|Back up|Clean|Mount|Unlink|Details/i.test(t));
      }
      return out;
    });
    await cleanup();
  }

  if (on('sqlite')) {
    await flow(port, 'alice', async (h) => {
      const uuid = await createDb('sqlite', `pages-sqlite-${h.label}`);
      const out = {};
      const row = () => dbRow('sqlite', uuid, 'description, image, sqlite_databases, custom_docker_run_options, is_log_drain_enabled');
      await h.go(`${URL_BASE}/${uuid}`);
      await h.shot('dbf-sqlite');
      await h.button('Details').click();
      await h.page.waitForTimeout(300);
      await h.shot('dbf-sqlite-details', false);
      await h.page.keyboard.press('Escape');
      await h.go(`${URL_BASE}/${uuid}`);
      await h.field('File names').fill('bad name.sqlite');
      await saveBar(h);
      out.bad = await h.toasts();
      out.bad_error = await h.page.locator('.text-red-500:visible').allInnerTexts();
      await h.field('File names').fill(' one.sqlite , two.sqlite ');
      await h.field('Description').fill('hello');
      await saveBar(h);
      out.saved = await h.toasts();
      out.row = row();
      await h.go(`${URL_BASE}/${uuid}`);
      out.field = await h.field('File names').inputValue();
      await h.listbox('Log drain', 'Forward logs');
      out.logdrain = await h.toasts();
      // Connect without choosing an application.
      await h.go(`${URL_BASE}/${uuid}`);
      await h.button('Mount volume').click();
      out.connect_err = (await h.toasts()).concat(await h.page.locator('.text-red-500:visible').allInnerTexts());
      // Backups page of SQLite.
      await h.go(`${URL_BASE}/${uuid}/backups`);
      await h.shot('dbf-sqlite-backups');
      await h.go(`${URL_BASE}/${uuid}/import-backup`);
      await h.shot('dbf-sqlite-import');
      return out;
    });
    await cleanup();
  }
} finally {
  await cleanup();
}
