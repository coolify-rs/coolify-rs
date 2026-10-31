// Shared variables pages: same steps on both UIs, comparing toasts, rows and audit_events.
//   node compat/ui/flow_shared_variables.mjs [port]
import { flow, psql, auditMax, auditSince } from './flow.mjs';

const port = process.argv[2] ?? '18086';
const PROJECT = 'j8xm2emlllhfnyebjzgyzp7z';
const ENV = 'uj3st9yvtvzxh7rul3c3opcz';
const SERVER = 'hml20fsj5eles0xfiqidq8j5';
const PREFIX = 'FLOW_';

const rows = () =>
  JSON.parse(
    psql(
      `select coalesce(json_agg(r order by r.key, r.type), '[]') from (select key, type, comment, is_multiline, is_literal, is_shown_once, team_id, project_id, environment_id, server_id from shared_environment_variables where key like '${PREFIX}%') r`,
    ),
  );
const cleanup = () => psql(`delete from shared_environment_variables where key like '${PREFIX}%'`);
const audit = (start) => auditSince(start, '^ui\\.shared_environment_variable');

async function openAdd(h) {
  await h.page.locator('button:visible', { hasText: 'Add variable' }).first().click();
  await h.page.waitForTimeout(400);
}

async function fillAdd(h, key, value, comment) {
  await h.page.locator('input[placeholder="NODE_ENV"]:visible').fill(key);
  await h.page.locator('input[placeholder="production"]:visible').fill(value);
  if (comment) await h.page.locator('input[placeholder="Optional note"]:visible').fill(comment);
}

async function submitAdd(h) {
  await h.page.locator('form:visible button[type=submit]:visible', { hasText: 'Add variable' }).click();
  await h.page.waitForTimeout(900);
}

/** The whole life of one variable on one scope page. */
async function scopeFlow(h, path, tag, rec) {
  await h.go(path);
  await h.shot(`sf-${tag}-empty`);
  await openAdd(h);
  await h.shot(`sf-${tag}-add`, false);
  await fillAdd(h, `${PREFIX}${tag.toUpperCase()}`, 'secret$value', 'note');
  await submitAdd(h);
  rec[`${tag}_add_toast`] = await h.toasts(600);
  rec[`${tag}_add_rows`] = rows();
  await h.shot(`sf-${tag}-row`);
  // Duplicate key.
  await openAdd(h);
  await fillAdd(h, `${PREFIX}${tag.toUpperCase()}`, 'x', '');
  await submitAdd(h);
  rec[`${tag}_dup_toast`] = await h.toasts(600);
  await h.shot(`sf-${tag}-dup`);
  // Edit modal.
  await h.page.locator('.env-table-item:visible').first().locator('.icon-button').last().click();
  await h.page.waitForTimeout(900);
  await h.shot(`sf-${tag}-edit`, false);
  if (tag === 'team') {
    await h.field('Name').fill('1 BAD');
    await h.page.locator('button[type=submit]:visible', { hasText: 'Update variable' }).click();
    await h.page.waitForTimeout(800);
    rec.edit_invalid = (await h.page.locator('.label-text-alt:visible').allInnerTexts()).join('|');
    rec.edit_invalid_toast = await h.toasts(200);
    await h.shot('sf-team-edit-invalid', false);
    await h.field('Name').fill(`${PREFIX}TEAM`);
  }
  await h.field('Comment').fill('edited');
  await h.page.locator('button[type=submit]:visible', { hasText: 'Update variable' }).click();
  rec[`${tag}_update_toast`] = await h.toasts(1200);
  rec[`${tag}_update_rows`] = rows();
  await h.page.waitForTimeout(300);
  // Developer view.
  await h.page.locator('button:visible', { hasText: 'Developer view' }).first().click();
  await h.page.waitForTimeout(600);
  await h.shot(`sf-${tag}-dev`);
  rec[`${tag}_dev_text`] = await h.page.locator('textarea:visible').first().inputValue();
  return rec;
}

console.log('== scopes');
await flow(
  port,
  'alice',
  async (h) => {
    const rec = {};
    const start = auditMax();
    await scopeFlow(h, '/shared-variables/team', 'team', rec);
    const area = h.page.locator('textarea:visible').first();
    const current = await area.inputValue();
    await area.fill(`${current}\n${PREFIX}BULK=one\n${PREFIX}BULK2="two # x" # c`);
    await h.page.locator('button:visible', { hasText: 'Save changes' }).first().click();
    rec.bulk_toast = await h.toasts(1200);
    rec.bulk_rows = rows();
    await h.go('/shared-variables/team');
    await h.page.locator('button:visible', { hasText: 'Developer view' }).first().click();
    await h.page.waitForTimeout(500);
    rec.bulk_text = await h.page.locator('textarea:visible').first().inputValue();
    await h.page.locator('textarea:visible').first().fill(rec.bulk_text.split('\n').filter((l) => !l.startsWith(`${PREFIX}BULK2`)).join('\n'));
    await h.page.locator('button:visible', { hasText: 'Save changes' }).first().click();
    rec.bulk_delete_toast = await h.toasts(1200);
    rec.bulk_delete_rows = rows();
    await scopeFlow(h, `/shared-variables/project/${PROJECT}`, 'project', rec);
    await scopeFlow(h, `/shared-variables/environments/project/${PROJECT}/environment/${ENV}`, 'environment', rec);
    await h.go(`/shared-variables/server/${SERVER}`);
    await h.shot('sf-server-empty');
    await openAdd(h);
    await fillAdd(h, `${PREFIX}SERVER`, 'v', '');
    await submitAdd(h);
    rec.server_rows = rows();
    await h.shot('sf-server-row');
    await h.page.locator('button:visible', { hasText: 'Developer view' }).first().click();
    await h.page.waitForTimeout(500);
    await h.shot('sf-server-dev');
    // Back on the team page: lock, then delete.
    await h.go('/shared-variables/team');
    await h.page.locator('.env-table-item:has-text("FLOW_TEAM")').locator('.icon-button').last().click();
    await h.page.waitForTimeout(800);
    await h.page.locator('button:visible', { hasText: /^\s*Lock\s*$/ }).first().click();
    rec.lock_toast = await h.toasts(1200);
    rec.lock_rows = rows();
    await h.page.waitForTimeout(400);
    await h.shot('sf-team-locked-edit', false);
    await h.page.locator('button:visible', { hasText: /^\s*Delete\s*$/ }).first().click();
    await h.page.waitForTimeout(400);
    await h.shot('sf-team-delete', false);
    await h.page.locator('input:visible[type=text], input:visible:not([type])').last().fill('FLOW_TEAM');
    await h.page.locator('button:visible', { hasText: 'Permanently Delete' }).last().click();
    rec.delete_toast = await h.toasts(1500);
    rec.delete_rows = rows();
    rec.audit = audit(start).map((r) => ({ ...r, metadata: { ...r.metadata, shared_environment_variable_uuid: null } }));
    return rec;
  },
  { before: cleanup, after: cleanup },
);

console.log('== lists and overview');
await flow(port, 'alice', async (h) => {
  await h.go('/shared-variables');
  await h.shot('sf-overview');
  await h.go('/shared-variables/projects');
  await h.shot('sf-projects');
  await h.go('/shared-variables/environments');
  await h.shot('sf-environments');
  await h.go('/shared-variables/servers');
  await h.shot('sf-servers');
  return { done: true };
});

console.log('== member');
const seeded = () =>
  psql(`insert into shared_environment_variables (key, value, type, team_id, created_at, updated_at) values ('${PREFIX}MEMBER', 'eyJpdiI6IiJ9', 'team', 4, now(), now())`);
await flow(
  port,
  'bob',
  async (h) => {
    await h.go('/shared-variables/team');
    await h.switchTeam("Bob Member's Team", 'Acme');
    await h.go('/shared-variables/team');
    await h.shot('sf-member');
    await h.page.locator('button:visible', { hasText: 'Developer view' }).first().click();
    await h.page.waitForTimeout(500);
    const text = await h.page.locator('textarea:visible').first().inputValue();
    await h.shot('sf-member-dev');
    return { text };
  },
  {
    before: () => {
      cleanup();
      seeded();
    },
    after: cleanup,
  },
);
