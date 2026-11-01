// Environment variables section of a resource: same steps on both UIs, each on its own application;
// compares toasts, rows and audit_events.
//   node compat/ui/flow_shared_envs.mjs <port> <refAppUuid> <rsAppUuid>
import { flow, psql, auditMax, auditSince } from './flow.mjs';

const [port, refApp, rsApp] = process.argv.slice(2);
const P = 'j8xm2emlllhfnyebjzgyzp7z';
const E = 'uj3st9yvtvzxh7rul3c3opcz';
const path = (app) => `/project/${P}/environment/${E}/application/${app}/environment-variables`;
const appId = (app) => psql(`select id from applications where uuid = '${app}'`);

const rows = (app) =>
  JSON.parse(
    psql(
      `select coalesce(json_agg(r order by r.is_preview, r.key), '[]') from (select key, comment, is_preview, is_shown_once, is_multiline, is_literal, is_required, is_shared, is_runtime, is_buildtime, "order", version, resourceable_type, preserve_compose_null, uses_legacy_escaping, (value is not null) as has_value from environment_variables where resourceable_type = 'App\\Models\\Application' and resourceable_id = ${appId(app)}) r`,
    ),
  );

const settings = (app) =>
  JSON.parse(
    psql(
      `select row_to_json(r) from (select is_env_sorting_enabled, use_build_secrets, include_coolify_default_variables, redact_all_env_values_in_logs from application_settings where application_id = ${appId(app)}) r`,
    ),
  );

const audit = (start, app) =>
  auditSince(start, '^ui\\.(environment_variable|application\\.settings)').map((e) => {
    const m = e.metadata ?? {};
    for (const k of Object.keys(m)) if (/uuid|name$/.test(k)) delete m[k];
    return { ...e, metadata: m, resource_name: e.resource_name === app ? 'APP' : e.resource_name, description: e.description.replace(app, 'APP') };
  });

const openAdd = async (h) => {
  await h.page.locator('button:visible', { hasText: /^\s*Add\s*$/ }).first().click();
  await h.page.waitForTimeout(500);
};
const fillAdd = async (h, key, value, comment) => {
  await h.page.locator('input[placeholder="NODE_ENV"]:visible').fill(key);
  await h.page.locator('input[placeholder="production"]:visible').fill(value);
  if (comment) await h.page.locator('input[placeholder="Optional note"]:visible').fill(comment);
};
const submitAdd = async (h) => {
  await h.page.locator('button[type=submit]:visible', { hasText: 'Add variable' }).click();
  await h.page.waitForTimeout(1000);
};
const row = (h, key) => h.page.locator(`.env-table-item[data-env-key="${key}"]:visible`).first();
const openEdit = async (h, key) => {
  await row(h, key).locator('[data-env-settings-trigger]').click();
  await h.page.waitForTimeout(1000);
};

await flow(port, 'alice', async (h) => {
  const app = h.label === 'ref' ? refApp : rsApp;
  const name = h.label === 'ref' ? 'shared-ref' : 'shared-rs';
  const rec = {};
  // Only this flow's own application.
  psql(`delete from environment_variables where resourceable_type = 'App\\Models\\Application' and resourceable_id = ${appId(app)}`);
  psql(`update application_settings set is_env_sorting_enabled = false, use_build_secrets = false, include_coolify_default_variables = true, redact_all_env_values_in_logs = true where application_id = ${appId(app)}`);
  await h.go(path(app));
  await h.page.waitForTimeout(800);
  await h.shot('se-empty');
  await openAdd(h);
  await h.shot('se-add', false);
  await fillAdd(h, 'NODE_ENV', 'production', 'note');
  await h.page.waitForTimeout(300);
  await h.shot('se-add-warning', false);
  let start = auditMax();
  await submitAdd(h);
  rec.add_toast = await h.toasts(600);
  rec.add_rows = rows(app);
  rec.add_audit = audit(start, name);
  await h.shot('se-row');

  // Bad key: stays in the modal with the message.
  await openAdd(h);
  await fillAdd(h, '1 BAD', 'x', '');
  await submitAdd(h);
  rec.bad_toast = await h.toasts(500);
  await h.shot('se-bad', false);
  await h.page.keyboard.press('Escape');
  await h.page.waitForTimeout(300);

  // Duplicate.
  await h.go(path(app));
  await h.page.waitForTimeout(800);
  await openAdd(h);
  await fillAdd(h, 'NODE_ENV', 'x', '');
  await submitAdd(h);
  rec.dup_toast = await h.toasts(600);

  // Edit modal, update, lock.
  await h.go(path(app));
  await h.page.waitForTimeout(800);
  await openEdit(h, 'NODE_ENV');
  await h.shot('se-edit', false);
  await h.page.locator('input[placeholder="Optional note"]:visible').fill('changed');
  await h.listbox('Interpolation', 'Literal');
  await h.page.waitForTimeout(200);
  start = auditMax();
  await h.page.locator('button[type=submit]:visible', { hasText: 'Update variable' }).click();
  rec.update_toast = await h.toasts(1000);
  rec.update_rows = rows(app);
  rec.update_audit = audit(start, name);
  await h.page.keyboard.press('Escape');
  await h.page.waitForTimeout(300);

  // Developer view: bulk.
  await h.page.locator('button:visible', { hasText: 'Developer view' }).first().click();
  await h.page.waitForTimeout(1000);
  rec.dev_text = await h.page.locator('textarea:visible').first().inputValue();
  await h.shot('se-dev');
  await h.page.locator('textarea:visible').first().fill('NODE_ENV=development #dev note\nBBB="quoted" # c\nAAA=1\nREF={{ team.NOPE }}\n');
  await h.page.waitForTimeout(300);
  await h.shot('se-dev-dirty');
  start = auditMax();
  await h.page.keyboard.press('Control+s').catch(() => {});
  await h.page.locator('button:visible', { hasText: /^\s*Save/ }).first().click();
  rec.bulk_toast = await h.toasts(1200);
  rec.bulk_rows = rows(app);
  rec.bulk_audit = audit(start, name);
  rec.dev_after = await h.page.locator('textarea:visible').first().inputValue();
  await h.page.locator('button:visible', { hasText: 'Normal view' }).first().click();
  await h.page.waitForTimeout(1000);
  await h.shot('se-many');

  // Filter, sort, search.
  await h.page.locator('.table-filter button').first().click();
  await h.page.waitForTimeout(200);
  await h.shot('se-filter-open', false);
  await h.page.locator('.listbox-option:visible', { hasText: 'Literal' }).first().click();
  await h.page.waitForTimeout(800);
  rec.filter_keys = await h.page.locator('.env-table-item:visible .env-key-label').allInnerTexts();
  await h.shot('se-filtered');
  await h.page.locator('.listbox-option:visible', { hasText: 'Reset filters' }).first().click({ timeout: 2000 }).catch(() => {});
  await h.page.keyboard.press('Escape');
  await h.page.waitForTimeout(600);
  await h.page.locator('.table-sort button').first().click();
  await h.page.waitForTimeout(200);
  await h.page.locator('.listbox-option:visible', { hasText: 'Name Z' }).first().click();
  await h.page.waitForTimeout(800);
  rec.sorted_keys = await h.page.locator('.env-table-item:visible .env-key-label').allInnerTexts();
  await h.page.locator('input[type=search]:visible').first().fill('zzz');
  await h.page.waitForTimeout(1000);
  await h.shot('se-search-empty');
  await h.page.locator('input[type=search]:visible').first().fill('');
  await h.page.waitForTimeout(800);

  // Settings.
  start = auditMax();
  await h.listbox('Environment variable order', 'Alphabetical');
  rec.sort_toast = await h.toasts(900);
  await h.listbox('Environment values in logs', 'Hide locked values only');
  rec.redact_toast = await h.toasts(900);
  await h.page.locator('label:visible', { hasText: 'default variables' }).first().click();
  rec.defaults_toast = await h.toasts(900);
  rec.settings = settings(app);
  rec.settings_audit = audit(start, name);
  await h.go(path(app));
  await h.page.waitForTimeout(900);
  rec.sorted_setting_keys = await h.page.locator('.env-table-item:visible .env-key-label').allInnerTexts();
  await h.shot('se-settings');

  // Lock.
  await openEdit(h, 'AAA');
  start = auditMax();
  await h.page.locator('button:visible', { hasText: /^\s*Lock\s*$/ }).first().click();
  await h.page.waitForTimeout(1000);
  await h.shot('se-locked-modal', false);
  rec.lock_toast = await h.toasts(300);
  rec.lock_audit = audit(start, name);
  await h.page.keyboard.press('Escape');
  await h.page.waitForTimeout(300);
  rec.lock_rows = rows(app);

  // Delete.
  await h.go(path(app));
  await h.page.waitForTimeout(900);
  await openEdit(h, 'BBB');
  await h.page.locator('[data-environment-variable-delete-action] button:visible', { hasText: 'Delete' }).first().click();
  await h.page.waitForTimeout(400);
  await h.shot('se-delete', false);
  for (let i = 0; i < 3; i++) {
    const check = h.page.locator('input[type=checkbox]:visible');
    const n = await check.count();
    for (let c = 0; c < n; c++) await check.nth(c).check({ force: true }).catch(() => {});
    const confirmInput = h.page.locator('input[placeholder="BBB"]:visible, input[type=text]:visible').last();
    await confirmInput.fill('BBB').catch(() => {});
    const go = h.page.locator('button:visible', { hasText: /Permanently Delete|^\s*Continue\s*$/ }).last();
    await go.click({ timeout: 1500 }).catch(() => {});
    await h.page.waitForTimeout(500);
    if (!(await h.page.locator('button:visible', { hasText: /Permanently Delete|^\s*Continue\s*$/ }).count())) break;
  }
  start = auditMax();
  rec.delete_toast = await h.toasts(900);
  rec.delete_rows = rows(app);
  return rec;
});
