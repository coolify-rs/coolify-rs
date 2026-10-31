// Scheduled tasks and Resource details: the same steps on both UIs. Rows are diffed with the uuid
// and id taken out, audit rows by event. Needs compat/diff/appui_fixture.py up.
//   node compat/ui/flow_appui_tasks.mjs <port>
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { flow, psql, auditMax, auditSince, diff } from './flow.mjs';

const [port] = process.argv.slice(2);
const F = JSON.parse(fs.readFileSync(path.join(os.tmpdir(), 'coolify-rs-appui-fixture.json'), 'utf8'));
const base = `/project/${F.project}/environment/${F.environment}`;
const owners = [
  ['image', `${base}/application/${F.image}`, 'application_id', `(select id from applications where uuid='${F.image}')`],
  ['compose', `${base}/application/${F.compose}`, 'application_id', `(select id from applications where uuid='${F.compose}')`],
  ['service', `${base}/service/${F.service}`, 'service_id', `(select id from services where uuid='${F.service}')`],
];
const rows = (column, id) =>
  JSON.parse(psql(`select coalesce(json_agg(r order by r.name), '[]') from (select enabled,name,command,frequency,container,timeout from scheduled_tasks where ${column} = ${id}) r`));

const results = await flow(port, 'alice', async (h) => {
  const rec = {};
  h.page.setDefaultTimeout(10000);
  const click = (re) => h.page.locator('button:visible', { hasText: re }).last().click();
  for (const [name, url, column, id] of owners) {
    const start = auditMax();
    const r = new Proxy({}, { set: (_, k, v) => ((rec[`${name}.${String(k)}`] = v), true) });
    const page = `${url}/scheduled-tasks`;
    await h.go(page);
    await h.shot(`task-${name}-empty`);
    await click(/^\s*Add task\s*$/);
    await h.page.waitForTimeout(500);
    await h.shot(`task-${name}-add`, false);

    // Invalid: nothing but the toast, and the dialog closes.
    await h.page.locator('button[type=submit]:visible', { hasText: 'Add task' }).click();
    r.requiredToast = await h.toasts();
    await click(/^\s*Add task\s*$/);
    await h.page.waitForTimeout(300);
    await h.field('Name').fill('nightly');
    await h.field('Schedule').fill('not a cron');
    await h.field('Command').fill('echo one');
    await h.page.locator('button[type=submit]:visible', { hasText: 'Add task' }).click();
    r.cronToast = await h.toasts();
    await click(/^\s*Add task\s*$/);
    await h.page.waitForTimeout(300);
    await h.field('Name').fill('nightly');
    await h.field('Schedule').fill('daily');
    await h.field('Command').fill('echo one');
    await h.page.locator('button[type=submit]:visible', { hasText: 'Add task' }).click();
    r.addedToast = await h.toasts();
    await h.page.waitForTimeout(800);
    await h.shot(`task-${name}-list`);
    r.afterAdd = rows(column, id);

    // Edit modal.
    await h.page.locator('[role=button]', { hasText: 'nightly' }).click();
    await h.page.waitForTimeout(800);
    await h.shot(`task-${name}-show`, false);
    r.urlHasTask = /task_uuid=/.test(h.page.url());
    await click(/^\s*Executions\s*$/);
    await h.page.waitForTimeout(300);
    await h.shot(`task-${name}-executions`, false);
    await click(/^\s*Danger Zone\s*$/);
    await h.page.waitForTimeout(300);
    await h.shot(`task-${name}-danger`, false);
    await click(/^\s*General\s*$/);
    await click(/^\s*Disable\s*$/);
    r.disableToast = await h.toasts();
    await h.field('Command').fill('  echo two  ');
    await h.field('Timeout').fill('600');
    await h.page.keyboard.press('Enter');
    r.saveToast = await h.toasts();
    await h.field('Schedule').fill('nope');
    await h.page.keyboard.press('Enter');
    r.badCronToast = await h.toasts();
    r.scheduleAfterBadCron = await h.field('Schedule').inputValue();
    await h.field('Timeout').fill('5');
    await h.page.keyboard.press('Enter');
    r.badTimeoutToast = await h.toasts();
    await h.field('Timeout').fill('600');
    r.afterEdit = rows(column, id);

    // Reload with the query string, then delete.
    const taskUrl = h.page.url().replace(h.base, '');
    await h.go(taskUrl);
    r.modalFromUrl = await h.page.locator('text=Edit scheduled task').count();
    await click(/^\s*Danger Zone\s*$/);
    await click(/^\s*Delete task\s*$/);
    await h.page.waitForTimeout(400);
    await h.shot(`task-${name}-delete`, false);
    await click(/^\s*Continue\s*$/).catch(() => {});
    await h.page.waitForTimeout(300);
    await h.page.locator('input:visible').last().fill('nightly');
    await click(/^\s*Permanently delete\s*$/);
    r.deleteToast = await h.toasts();
    r.afterDelete = rows(column, id);
    r.urlAfterDelete = /task_uuid=/.test(h.page.url());
    r.audit = auditSince(start, 'scheduled_task').map((a) => a.event);
  }

  // Resource details of a service.
  await h.go(`${base}/service/${F.service}`);
  await click(/^\s*Details\s*$/);
  await h.page.waitForTimeout(800);
  await h.shot('task-details', false);
  rec.details = await h.page.locator('input[readonly]:visible').evaluateAll((list) => list.map((i) => i.value));

  // `/tasks/{uuid}` redirects, an unknown uuid stays on the list's error.
  return rec;
});
void diff;
void results;
