// Persistent storage section: same steps on both UIs, each on its own application; compares toasts,
// rows, audit_events and the files on the test VM.
//   node compat/ui/flow_shared_storages.mjs <port> <refAppUuid> <rsAppUuid>
import { execFileSync } from 'node:child_process';
import { flow, psql, auditMax, auditSince } from './flow.mjs';

const [port, refApp, rsApp] = process.argv.slice(2);
const P = 'j8xm2emlllhfnyebjzgyzp7z';
const E = 'uj3st9yvtvzxh7rul3c3opcz';
const path = (app) => `/project/${P}/environment/${E}/application/${app}/persistent-storage`;
const appId = (app) => psql(`select id from applications where uuid = '${app}'`);
const vm = (cmd) => execFileSync('orb', ['-m', 'coolify-rs-e2e', '-u', 'root', 'bash', '-c', cmd], { encoding: 'utf8' }).trim();
const norm = (s, app) => JSON.parse(JSON.stringify(s).replaceAll(app, 'APP'));

const rows = (app) =>
  norm(
    {
      volumes: JSON.parse(psql(`select coalesce(json_agg(r order by r.mount_path), '[]') from (select name, mount_path, host_path, is_preview_suffix_enabled, resource_type from local_persistent_volumes where resource_type = 'App\\Models\\Application' and resource_id = ${appId(app)}) r`)),
      files: JSON.parse(psql(`select coalesce(json_agg(r order by r.mount_path), '[]') from (select fs_path, mount_path, is_directory, is_host_file, is_based_on_git, is_preview_suffix_enabled, chown, chmod, resource_type, (content is not null) as has_content from local_file_volumes where resource_type = 'App\\Models\\Application' and resource_id = ${appId(app)}) r`)),
    },
    app,
  );

const audit = (start, app, name) =>
  norm(
    auditSince(start, '^ui\\.application\\.storage').map((e) => {
      const m = e.metadata ?? {};
      for (const k of ['storage_uuid', 'storage_id', 'application_uuid', 'application_name']) delete m[k];
      return { event: e.event, description: e.description.replace(name, 'APP'), metadata: m };
    }),
    app,
  );

const mountMenu = async (h, item) => {
  await h.page.locator('button:visible', { hasText: 'Add mount' }).first().click();
  await h.page.waitForTimeout(200);
  await h.page.locator('[role=menuitem]:visible', { hasText: item }).first().click();
  await h.page.waitForTimeout(400);
};
const submitModal = async (h, text) => {
  await h.page.locator('button[type=submit]:visible', { hasText: text }).first().click();
};
const LABELS = {
  name: 'Name',
  mount_path: 'Destination Path',
  file_storage_path: 'Destination Path',
  host_file_storage_source: 'Host File Path',
  host_file_storage_destination: 'Destination Path',
  file_storage_directory_source: 'Source Directory',
  file_storage_directory_destination: 'Destination Directory',
};
const inputAfter = (h, id) => (id === 'file_storage_content' ? h.page.locator('textarea:visible').last() : h.page.locator('label:visible', { hasText: LABELS[id] }).last().locator('xpath=ancestor::div[.//input][1]//input').first());
const tab = async (h, name) => {
  await h.page.locator('button:visible', { hasText: new RegExp(`^\\s*${name} \\(`) }).first().click();
  await h.page.waitForTimeout(700);
};
/** Runs a ConfirmModal to the end: boxes, typed name and password. */
const confirmAll = async (h, text) => {
  for (let i = 0; i < 4; i++) {
    const boxes = h.page.locator('input[type=checkbox]:visible');
    for (let c = 0; c < (await boxes.count()); c++) await boxes.nth(c).check({ force: true }).catch(() => {});
    const go = h.page.locator('button:visible', { hasText: /^\s*(Continue|Confirm|Permanently Delete|Convert to file|Convert to directory)\s*$/ }).last();
    const pwd = h.page.locator('input[type=password]:visible').last();
    if (await pwd.count()) await pwd.fill(h.secret).catch(() => {});
    const typed = h.page.locator('input[type=text]:visible:not([readonly]):not([disabled])').last();
    if (await typed.count()) await typed.fill(text).catch(() => {});
    await go.click({ timeout: 1500 }).catch(() => {});
    await h.page.waitForTimeout(800);
    if (!(await h.page.locator('button:visible', { hasText: /^\s*Cancel\s*$/ }).count())) break;
  }
};

await flow(port, 'alice', async (h) => {
  const app = h.label === 'ref' ? refApp : rsApp;
  const name = h.label === 'ref' ? 'shared-ref' : 'shared-rs';
  const dir = `/data/coolify/applications/${app}`;
  const rec = {};
  psql(`delete from local_persistent_volumes where resource_type = 'App\\Models\\Application' and resource_id = ${appId(app)}`);
  psql(`delete from local_file_volumes where resource_type = 'App\\Models\\Application' and resource_id = ${appId(app)}`);
  vm(`rm -rf ${dir}`);
  await h.go(path(app));
  await h.page.waitForTimeout(600);
  await h.shot('st-empty');
  await h.page.locator('button:visible', { hasText: 'Add mount' }).first().click();
  await h.page.waitForTimeout(250);
  await h.shot('st-menu', false);
  await h.page.keyboard.press('Escape');

  // Volume.
  await h.go(path(app));
  await h.page.waitForTimeout(600);
  await mountMenu(h, 'Volume mount');
  rec.default_name = norm(await inputAfter(h, 'name').inputValue(), app);
  await h.shot('st-volume-modal', false);
  await inputAfter(h, 'name').fill('-bad');
  await inputAfter(h, 'mount_path').fill('/data');
  await submitModal(h, 'Add volume');
  rec.volume_bad_toast = await h.toasts(800);
  await h.shot('st-volume-bad', false);
  await inputAfter(h, 'name').fill('vol-a');
  let start = auditMax();
  await submitModal(h, 'Add volume');
  rec.volume_toast = await h.toasts(1200);
  rec.volume_audit = audit(start, app, name);
  await h.page.keyboard.press('Escape');
  await h.page.waitForTimeout(300);
  rec.after_volume = rows(app);
  await h.shot('st-volume-row');

  // Update the volume row.
  await h.go(path(app));
  await h.page.waitForTimeout(800);
  await h.page.locator('.env-table-item input:visible').nth(1).fill('/data2');
  start = auditMax();
  await h.page.locator('.env-table-item button[type=submit]:visible').first().click();
  rec.update_toast = await h.toasts(1000);
  rec.update_audit = audit(start, app, name);
  rec.after_update = rows(app);

  // File mount: written on the server.
  await mountMenu(h, 'File mount');
  await inputAfter(h, 'file_storage_path').fill('/etc/app/conf.ini');
  await h.page.waitForTimeout(200);
  await h.shot('st-file-modal', false);
  await inputAfter(h, 'file_storage_content').fill('a=1\nb=2\n');
  start = auditMax();
  await submitModal(h, 'Add file');
  rec.file_toast = await h.toasts(1500);
  rec.file_audit = audit(start, app, name);
  await h.page.keyboard.press('Escape');
  await h.page.waitForTimeout(3000);
  rec.after_file = rows(app);
  rec.vm_file = vm(`cat ${dir}/etc/app/conf.ini; stat -c '%a %F' ${dir}/etc/app/conf.ini`);

  // Host file mount.
  await mountMenu(h, 'Host file mount');
  await inputAfter(h, 'host_file_storage_source').fill('/etc/hostname');
  await inputAfter(h, 'host_file_storage_destination').fill('/host/name');
  await h.shot('st-hostfile-modal', false);
  await submitModal(h, 'Add host file');
  rec.host_toast = await h.toasts(1200);
  await h.page.keyboard.press('Escape');
  await h.page.waitForTimeout(400);
  rec.after_host = rows(app);
  await h.shot('st-files', true);

  // Directory mount.
  await mountMenu(h, 'Directory mount');
  rec.dir_default = norm(await inputAfter(h, 'file_storage_directory_source').inputValue(), app);
  await inputAfter(h, 'file_storage_directory_source').fill(`${dir}/mydir`);
  await inputAfter(h, 'file_storage_directory_destination').fill('/mnt/dir');
  await h.shot('st-dir-modal', false);
  await submitModal(h, 'Add directory');
  rec.dir_toast = await h.toasts(2000);
  await h.page.keyboard.press('Escape');
  await h.page.waitForTimeout(2500);
  rec.after_dir = rows(app);
  rec.vm_dir = vm(`test -d ${dir}/mydir && echo dir || echo none`);
  await h.shot('st-directories');

  // Edit a file's content.
  await h.go(path(app));
  await h.page.waitForTimeout(800);
  await tab(h, 'Files');
  const ta = h.page.locator('textarea:visible').first();
  rec.loaded_content = await ta.inputValue();
  await ta.fill('a=9\n');
  await h.page.waitForTimeout(300);
  await h.shot('st-file-dirty');
  await h.page.locator('button:visible', { hasText: /Save changes/ }).first().click();
  rec.save_toast = await h.toasts(2500);
  rec.vm_saved = vm(`cat ${dir}/etc/app/conf.ini`);

  // Load from server after changing the file on the VM.
  vm(`printf 'c=3' > ${dir}/etc/app/conf.ini`);
  await h.page.locator('button:visible', { hasText: 'Load from server' }).first().click();
  rec.load_toast = await h.toasts(2500);
  rec.after_load = await h.page.locator('textarea:visible').first().inputValue();

  // Convert the file to a directory.
  await h.page.locator('button:visible', { hasText: 'Convert to directory' }).first().click();
  await h.page.waitForTimeout(400);
  await h.shot('st-convert-modal', false);
  await confirmAll(h, `${dir}/etc/app/conf.ini`);
  await h.page.waitForTimeout(2500);
  rec.after_convert = rows(app);
  rec.vm_converted = vm(`test -d ${dir}/etc/app/conf.ini && echo dir || echo none`);

  // Delete the directory and its contents from the server.
  await h.go(path(app));
  await h.page.waitForTimeout(800);
  await tab(h, 'Directories');
  await h.page.locator('button:visible', { hasText: /^\s*Delete\s*$/ }).first().click();
  await h.page.waitForTimeout(400);
  await h.shot('st-delete-modal', false);
  start = auditMax();
  await confirmAll(h, `${dir}/etc/app/conf.ini`);
  rec.delete_toast = await h.toasts(2000);
  rec.delete_audit = audit(start, app, name);
  rec.after_delete = rows(app);
  rec.vm_deleted = vm(`test -e ${dir}/etc/app/conf.ini && echo there || echo gone`);

  // Delete the volume.
  await h.go(path(app));
  await h.page.waitForTimeout(800);
  await tab(h, 'Volumes');
  await h.page.locator('.env-table-item button:visible', { hasText: /^\s*Delete\s*$/ }).first().click();
  await h.page.waitForTimeout(400);
  await h.shot('st-volume-delete', false);
  const volName = `${app}-vol-a`;
  await confirmAll(h, volName);
  rec.volume_delete_toast = await h.toasts(1200);
  rec.after_volume_delete = rows(app);
  return rec;
});
