// File browser, Destination and Maintenance: the same steps on both UIs on running resources.
// Compares toasts, listings, files inside the containers and the maintenance files on the test VM.
//   node compat/ui/flow_files.mjs <port>
// Everything created is tracked by uuid and removed at the end, also after a failure.
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import { flow, psql, tokens } from './flow.mjs';

const [port] = process.argv.slice(2);
const SERVER = 'hml20fsj5eles0xfiqidq8j5';
const REF = 'http://127.0.0.1:18000';
const vm = (cmd) => execFileSync('orb', ['-m', 'coolify-rs-e2e', '-u', 'root', 'bash', '-c', cmd], { encoding: 'utf8' }).trim();
const api = async (method, path, body) => {
  const r = await fetch(`${REF}/api/v1${path}`, {
    method,
    headers: { Authorization: `Bearer ${tokens.acme_owner_root}`, 'Content-Type': 'application/json', Accept: 'application/json' },
    body: body ? JSON.stringify(body) : undefined,
  });
  const text = await r.text();
  if (!r.ok) throw new Error(`${method} ${path}: ${r.status} ${text.slice(0, 300)}`);
  return text ? JSON.parse(text) : {};
};
const stamp = Date.now().toString(36);
const P = (await api('POST', '/projects', { name: `files-${stamp}` })).uuid;
const E = (await api('GET', `/projects/${P}/environments`))[0].uuid;
const db = (await api('POST', '/databases/redis', { project_uuid: P, server_uuid: SERVER, environment_name: 'production', name: 'files-db', instant_deploy: true })).uuid;
const app = (await api('POST', '/applications/dockerimage', { project_uuid: P, server_uuid: SERVER, environment_name: 'production', docker_registry_image_name: 'nginx', docker_registry_image_tag: 'alpine', ports_exposes: '80', name: 'files-app', domains: `http://files-${stamp}.example.test`, instant_deploy: false })).uuid;
const compose = Buffer.from('services:\n  web:\n    image: nginx:alpine\n').toString('base64');
const service = (await api('POST', '/services', { project_uuid: P, server_uuid: SERVER, environment_name: 'production', name: 'files-svc', docker_compose_raw: compose, instant_deploy: false })).uuid;

const waitRunning = async () => {
  for (let i = 0; i < 40; i++) {
    const s = psql(`select status from standalone_redis where uuid = '${db}'`);
    const up = vm(`docker ps --filter name=${db} --format '{{.State}}'`);
    if (s.startsWith('running') && up === 'running') return true;
    await new Promise((r) => setTimeout(r, 3000));
  }
  return false;
};

const dir = (seg, uuid) => `/project/${P}/environment/${E}/${seg}/${uuid}`;
const inContainer = (cmd) => vm(`docker exec ${db} sh -c '${cmd}'`);
const local = new URL('./shots/files-upload.txt', import.meta.url).pathname;
fs.writeFileSync(local, 'uploaded content\n');

const row = (h, name) => h.page.locator('.data-table-row.file-table-grid:visible', { hasText: new RegExp(`(^|\\s)${name.replace('.', '\\.')}(\\s|$)`) }).first();
const submit = async (h, text) => h.page.locator('button[type=submit]:visible', { hasText: text }).last().click({ force: true });
const newMenu = async (h, item) => {
  await h.page.locator('button:visible', { hasText: /^\s*New\s*$/ }).first().click({ force: true });
  await h.page.waitForTimeout(200);
  await h.page.locator('.listbox-option:visible', { hasText: item }).first().click({ force: true });
  await h.page.waitForTimeout(300);
};
const rowAction = async (h, name, item) => {
  await row(h, name).locator('button[aria-label="Actions"], button[title="Actions"]').first().click({ force: true, timeout: 8000 });
  await h.page.waitForTimeout(200);
  await h.page.locator('.listbox-option:visible', { hasText: item }).first().click({ force: true });
  await h.page.waitForTimeout(500);
};
const listing = async (h) => (await h.page.locator('.data-table-row').allInnerTexts()).map((t) => t.replace(/\s+/g, ' ').replace(/\d{4}-\d\d-\d\d \d\d:\d\d:\d\d/, 'T').trim());

try {
  rec_ok: if (!(await waitRunning())) {
    console.log('the database did not reach running; file browser comparison skipped');
    break rec_ok;
  }
  inContainer('rm -rf /data/ops; mkdir -p /data/ops');
  await flow(port, 'alice', async (h) => {
    const rec = {};
    const name = `dir-${h.label}`;
    await h.go(`${dir('database', db)}/files`);
    await h.page.waitForTimeout(2500);
    await h.shot('files-db');
    rec.path = await h.page.locator('.flex.flex-wrap.items-center.px-4').first().innerText().catch(() => '');
    rec.listing_root = await listing(h);

    // Into /data/ops.
    await h.page.locator('.data-table-row button', { hasText: /^\s*ops\s*$/ }).first().click({ force: true }).catch(() => {});
    await h.page.waitForTimeout(1500);
    rec.path_ops = await h.page.locator('.flex.flex-wrap.items-center.px-4').first().innerText().catch(() => '');
    await h.shot('files-empty');

    await newMenu(h, 'New folder');
    await h.shot('files-new-folder', false);
    await h.field('Folder name').fill('sub');
    await submit(h, 'Create folder');
    rec.toast_folder = await h.toasts(1500);
    await newMenu(h, 'New file');
    await h.field('File name').fill('a.yml');
    await submit(h, 'Create file');
    rec.toast_file = await h.toasts(1500);
    rec.listing = await listing(h);
    await h.shot('files-list');

    // Edit.
    await rowAction(h, 'a.yml', 'Edit');
    await h.page.waitForTimeout(2500);
    await h.shot('files-editor', false);
    await h.page.evaluate(() => window.monaco?.editor.getEditors()[0]?.setValue('key: value\n'));
    await h.page.waitForTimeout(300);
    await h.page.locator('button[type=submit]:visible', { hasText: 'Save changes' }).last().evaluate((b) => b.click());
    rec.toast_save = await h.toasts(1500);
    rec.saved = inContainer('cat /data/ops/a.yml');

    // Permissions.
    await h.page.keyboard.press('Escape');
    await h.page.waitForTimeout(800);
    await h.shot('files-after-save', false);
    await row(h, 'a.yml').locator('button[title="Edit permissions"]').evaluate((b) => b.click());
    await h.page.waitForTimeout(300);
    await h.field('Octal mode').fill('640', { timeout: 8000 });
    await h.shot('files-chmod', false);
    await submit(h, 'Apply');
    rec.toast_chmod = await h.toasts(1500);
    rec.mode = inContainer('stat -c %a /data/ops/a.yml');
    await h.page.waitForTimeout(800);
    await row(h, 'a.yml').locator('button[title="Edit permissions"]').evaluate((b) => b.click());
    await h.page.waitForTimeout(300);
    await h.field('Octal mode').fill('abc', { timeout: 8000 });
    await submit(h, 'Apply');
    rec.toast_chmod_bad = await h.toasts(1500);

    // Rename and upload.
    await rowAction(h, 'a.yml', 'Rename');
    await h.field('New name').fill('b.yml');
    await submit(h, 'Rename');
    rec.toast_rename = await h.toasts(1500);
    const [chooser] = await Promise.all([h.page.waitForEvent('filechooser', { timeout: 5000 }).catch(() => null), h.page.locator('button:visible', { hasText: /^\s*Upload\s*$/ }).first().click({ force: true })]);
    if (chooser) await chooser.setFiles(local);
    rec.toast_upload = await h.toasts(2500);
    rec.uploaded = inContainer('cat /data/ops/files-upload.txt');

    // Download.
    const [dl] = await Promise.all([h.page.waitForEvent('download', { timeout: 10000 }).catch(() => null), rowAction(h, 'files-upload.txt', 'Download')]);
    rec.download_name = dl?.suggestedFilename() ?? null;
    if (dl) rec.download = fs.readFileSync(await dl.path(), 'utf8');
    const [dl2] = await Promise.all([h.page.waitForEvent('download', { timeout: 10000 }).catch(() => null), rowAction(h, 'sub', 'Download')]);
    rec.download_dir_name = dl2?.suggestedFilename() ?? null;
    rec.listing_after = await listing(h);

    // Delete.
    for (const n of ['b.yml', 'files-upload.txt', 'sub']) {
      await rowAction(h, n, 'Delete');
      await h.shot('files-delete', false);
      await h.page.locator('button:visible', { hasText: /^\s*Delete\s*$/ }).last().click({ force: true });
      rec[`toast_delete_${n}`] = await h.toasts(1500);
    }
    rec.listing_end = inContainer('ls -A /data/ops | wc -l');

    // A resource that is not running.
    await h.go(`${dir('application', app)}/files`);
    await h.page.waitForTimeout(1500);
    await h.shot('files-stopped');
    rec.stopped = await h.page.locator('text=Container is not running').count();
    await h.go(`${dir('service', service)}/files`);
    await h.page.waitForTimeout(1500);
    await h.shot('files-svc-stopped');
    rec.stopped_svc = await h.page.locator('text=Container is not running').count();

    // Servers section.
    await h.go(`${dir('application', app)}/servers`);
    await h.page.waitForTimeout(1500);
    await h.shot('dest-app');
    rec.dest_app = (await h.page.locator('.application-settings-section:visible').allInnerTexts()).map((t) => t.replace(/\s+/g, ' '));
    await h.go(`${dir('database', db)}/servers`);
    await h.page.waitForTimeout(1500);
    await h.shot('dest-db');
    rec.dest_db = (await h.page.locator('.application-settings-section:visible').allInnerTexts()).map((t) => t.replace(/\s+/g, ' '));

    // Maintenance on the service page.
    await h.go(`${dir('service', service)}/domains`);
    await h.page.waitForTimeout(1500);
    await h.shot('maint-svc');
    rec.maint_section = await h.page.locator('#maintenance-section').innerText().then((t) => t.replace(/\s+/g, ' ')).catch(() => null);

    // Maintenance on the server: the same fixture, off again at the end of each side.
    const subId = psql(`select a.id from service_applications a join services s on s.id = a.service_id where s.uuid = '${service}'`);
    psql(`update service_applications set fqdn = 'https://maint-${stamp}.example.test/x,http://maint2-${stamp}.example.test' where id = ${subId}`);
    const yaml = '/data/coolify/proxy/dynamic/coolify-maintenance.yaml';
    const page = `/data/coolify/proxy/error-pages/maintenance-${service}.html`;
    const fileState = () => ({ yaml: vm(`cat ${yaml} 2>/dev/null | grep -v '^ *$' || true`), page: vm(`cat ${page} 2>/dev/null || true`) });
    await h.go(`${dir('service', service)}/domains`);
    await h.page.waitForTimeout(1500);
    await h.page.locator('#maintenance-section button.listbox-trigger').first().click({ force: true });
    await h.page.waitForTimeout(200);
    await h.page.locator('.listbox-option:visible', { hasText: 'On: show' }).first().click({ force: true });
    rec.maint_on_toast = await h.toasts(5000);
    await h.page.waitForTimeout(1500);
    rec.maint_on = fileState();
    rec.maint_on_row = psql(`select is_maintenance_enabled from services where uuid = '${service}'`);
    await h.shot('maint-on');
    await h.page.locator('#maintenance-section textarea').first().fill('<h1>custom</h1>');
    await h.page.waitForTimeout(300);
    await h.page.locator('button:visible', { hasText: /^\s*Save\s*$/ }).last().click({ force: true }).catch(() => {});
    rec.maint_save_toast = await h.toasts(5000);
    rec.maint_custom_page = vm(`cat ${page} 2>/dev/null || true`);
    rec.maint_custom_row = psql(`select maintenance_page from services where uuid = '${service}'`);
    await h.page.locator('button:visible', { hasText: 'Reset to default' }).first().click({ force: true });
    rec.maint_reset_toast = await h.toasts(5000);
    rec.maint_reset_page = vm(`cat ${page} 2>/dev/null | head -5 || true`);
    rec.maint_reset_row = psql(`select coalesce(maintenance_page, 'NULL') from services where uuid = '${service}'`);
    await h.page.locator('#maintenance-section button.listbox-trigger').first().click({ force: true });
    await h.page.waitForTimeout(200);
    await h.page.locator('.listbox-option:visible', { hasText: 'Off: serve' }).first().click({ force: true });
    rec.maint_off_toast = await h.toasts(5000);
    rec.maint_off = fileState();

    // Danger: an application with a file storage with content, on its own fixture.
    const victim = (await api('POST', '/applications/dockerimage', { project_uuid: P, server_uuid: SERVER, environment_name: 'production', docker_registry_image_name: 'nginx', docker_registry_image_tag: 'alpine', ports_exposes: '80', name: 'files-victim', instant_deploy: false })).uuid;
    const victimId = psql(`select id from applications where uuid = '${victim}'`);
    const APP = String.raw`App\Models\Application`;
    psql(`insert into local_persistent_volumes (uuid, name, mount_path, resource_type, resource_id, created_at, updated_at) values ('dv${h.label}${stamp}', '${victim}-vol', '/vol', '${APP}', ${victimId}, now(), now())`);
    psql(`insert into local_file_volumes (uuid, fs_path, mount_path, content, is_directory, resource_type, resource_id, created_at, updated_at) values ('df${h.label}${stamp}', '/data/coolify/applications/${victim}/conf.txt', '/etc/conf.txt', 'hello', false, '${APP}', ${victimId}, now(), now())`);
    vm(`mkdir -p /data/coolify/applications/${victim} && echo hello > /data/coolify/applications/${victim}/conf.txt && docker volume create ${victim}-vol >/dev/null`);
    await h.go(`${dir('application', victim)}/danger`);
    await h.page.waitForTimeout(1500);
    await h.shot('danger-app');
    await h.page.locator('button:visible', { hasText: /^\s*Delete application\s*$/ }).first().click({ force: true });
    await h.page.waitForTimeout(500);
    for (let i = 0; i < 5; i++) {
      const pwd = h.page.locator('input[type=password]:visible').last();
      if (await pwd.count()) await pwd.fill(h.secret).catch(() => {});
      const typed = h.page.locator('input[type=text]:visible:not([readonly]):not([disabled])').last();
      if (await typed.count()) await typed.fill('files-victim').catch(() => {});
      await h.page.locator('button:visible', { hasText: /^\s*(Continue|Confirm|Permanently Delete|Delete)\s*$/ }).last().click({ timeout: 1500, force: true }).catch(() => {});
      await h.page.waitForTimeout(900);
      if (!(await h.page.locator('input[type=password]:visible').count()) && i > 0) break;
    }
    await h.page.waitForTimeout(12000);
    rec.danger_url = new URL(h.page.url()).pathname.replace(P, 'P').replace(E, 'E');
    rec.danger_row = psql(`select count(*) from applications where uuid = '${victim}'`);
    rec.danger_files = psql(`select count(*) from local_file_volumes where resource_type = '${APP}' and resource_id = ${victimId}`);
    rec.danger_vols = psql(`select count(*) from local_persistent_volumes where resource_type = '${APP}' and resource_id = ${victimId}`);
    rec.danger_dir = vm(`ls /data/coolify/applications/${victim} 2>&1 | wc -l; docker volume ls -q --filter name=${victim}-vol | wc -l`);
    return rec;
  });
} finally {
  for (const [k, u] of [['databases', db], ['applications', app], ['services', service]]) await api('DELETE', `/${k}/${u}`).catch(() => {});
  await new Promise((r) => setTimeout(r, 25000));
  await api('DELETE', `/projects/${P}`).catch((e) => console.log('leftover project', P, e.message));
  fs.rmSync(local, { force: true });
}
