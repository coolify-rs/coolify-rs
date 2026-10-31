// Resource limits, healthcheck, tags and webhooks sections: same steps on both UIs, each on its own
// application; compares toasts, rows and audit_events.
//   node compat/ui/flow_shared_config.mjs <port> <refAppUuid> <rsAppUuid>
import { flow, psql, auditMax, auditSince } from './flow.mjs';

const [port, refApp, rsApp] = process.argv.slice(2);
const P = 'j8xm2emlllhfnyebjzgyzp7z';
const E = 'uj3st9yvtvzxh7rul3c3opcz';
const path = (app, s) => `/project/${P}/environment/${E}/application/${app}/${s}`;
const TAG_A = 'flowsharedaa';
const TAG_B = 'flowsharedbb';

const app_row = (app) =>
  JSON.parse(psql(`select row_to_json(r) from (select limits_cpus, limits_cpuset, limits_cpu_shares, limits_memory, limits_memory_swap, limits_memory_swappiness, limits_memory_reservation, health_check_enabled, health_check_type, health_check_command, health_check_method, health_check_scheme, health_check_host, health_check_port, health_check_path, health_check_return_code, health_check_response_text, health_check_interval, health_check_timeout, health_check_retries, health_check_start_period, (manual_webhook_secret_github is not null) as has_github from applications where uuid = '${app}') r`));
const tags = (app) =>
  JSON.parse(psql(`select coalesce(json_agg(t.name order by t.name), '[]') from taggables x join tags t on t.id = x.tag_id where x.taggable_type = 'App\\Models\\Application' and x.taggable_id = (select id from applications where uuid = '${app}')`));
const audit = (start, name) =>
  auditSince(start, '^ui\\.(application|tag)\\.').map((e) => {
    const m = e.metadata ?? {};
    for (const k of ['application_uuid', 'application_name', 'tag_uuid']) delete m[k];
    return { event: e.event, description: e.description.replace(name, 'APP'), metadata: m };
  });
const save = async (h) => {
  await h.page.locator('button:visible', { hasText: /Save changes/ }).first().click();
};
const field = (h, label) => h.page.locator('label:visible', { hasText: label }).first().locator('xpath=ancestor::div[.//input][1]//input').first();

const dropTags = () => {
  psql(`delete from taggables where tag_id in (select id from tags where team_id = 4 and name in ('${TAG_A}', '${TAG_B}'))`);
  psql(`delete from tags where team_id = 4 and name in ('${TAG_A}', '${TAG_B}')`);
};

await flow(port, 'alice', async (h) => {
  const app = h.label === 'ref' ? refApp : rsApp;
  const name = h.label === 'ref' ? 'shared-ref' : 'shared-rs';
  const rec = {};
  // Only this flow's own application and the two tags it creates.
  psql(`update applications set limits_cpus = '0', limits_cpuset = null, limits_cpu_shares = 1024, limits_memory = '0', limits_memory_swap = '0', limits_memory_swappiness = 60, limits_memory_reservation = '0', health_check_enabled = false, health_check_type = 'http', health_check_command = null, health_check_method = 'GET', health_check_scheme = 'http', health_check_host = 'localhost', health_check_port = null, health_check_path = '/', health_check_response_text = null, manual_webhook_secret_github = null where uuid = '${app}'`);

  // Limits.
  await h.go(path(app, 'resource-limits'));
  await h.page.waitForTimeout(500);
  await h.shot('sc-limits');
  await field(h, 'CPU limit').fill('abc');
  await field(h, 'Memory limit').fill('12x');
  await save(h);
  rec.limits_bad = await h.toasts(800);
  await field(h, 'CPU limit').fill('0.5');
  await field(h, 'CPU set').fill('0-1');
  await field(h, 'CPU weight').fill('512');
  await field(h, 'Memory limit').fill('256m');
  await field(h, 'Memory and swap limit').fill('512m');
  await field(h, 'Memory reservation').fill('128m');
  await field(h, 'Swappiness').fill('10');
  await h.page.waitForTimeout(200);
  await h.shot('sc-limits-dirty');
  let start = auditMax();
  await save(h);
  rec.limits_toast = await h.toasts(800);
  rec.limits_row = app_row(app);
  rec.limits_audit = audit(start, name);

  // Healthcheck.
  await h.go(path(app, 'healthcheck'));
  await h.page.waitForTimeout(500);
  await h.shot('sc-health');
  await h.page.locator('button:visible', { hasText: /^\s*Enable\s*$/ }).first().click();
  await h.page.waitForTimeout(400);
  await h.shot('sc-health-enable', false);
  start = auditMax();
  await h.page.locator('button:visible', { hasText: /^\s*Enable healthcheck\s*$/ }).last().click();
  rec.enable_toast = await h.toasts(1200);
  rec.enable_row = app_row(app);
  rec.enable_audit = audit(start, name);
  await h.shot('sc-health-on');
  await field(h, 'Path').fill('bad path!');
  await save(h);
  rec.health_bad = await h.toasts(800);
  await field(h, 'Path').fill('/ready');
  await field(h, 'Port').fill('8080');
  await field(h, 'Expected response text').fill('OK');
  await h.listbox('Scheme', 'HTTPS');
  await h.page.waitForTimeout(200);
  start = auditMax();
  await save(h);
  rec.health_toast = await h.toasts(800);
  rec.health_row = app_row(app);
  rec.health_audit = audit(start, name);
  await h.listbox('Check type', 'Container command');
  await h.page.waitForTimeout(400);
  await h.shot('sc-health-cmd');
  await field(h, 'Command').fill('curl localhost;rm');
  await save(h);
  rec.cmd_bad = await h.toasts(800);
  await field(h, 'Command').fill('pg_isready -U postgres');
  await save(h);
  rec.cmd_toast = await h.toasts(800);
  rec.cmd_row = app_row(app);
  await h.page.locator('button:visible', { hasText: /^\s*Disable\s*$/ }).first().click();
  rec.disable_toast = await h.toasts(1000);
  rec.disable_row = app_row(app);

  // Tags.
  await h.go(path(app, 'tags'));
  await h.page.waitForTimeout(500);
  await h.shot('sc-tags-empty');
  const input = field(h, 'Tag names');
  await input.fill(`${TAG_A} ${TAG_B} x`);
  start = auditMax();
  await h.page.locator('button[type=submit]:visible', { hasText: 'Add tags' }).first().click();
  rec.tags_toast = await h.toasts(1000);
  rec.tags_rows = tags(app);
  await h.shot('sc-tags');
  await input.fill(TAG_A);
  await h.page.locator('button[type=submit]:visible', { hasText: 'Add tags' }).first().click();
  rec.tags_dup = await h.toasts(1000);
  await h.page.locator('button:visible', { hasText: /^\s*Remove\s*$/ }).first().click();
  rec.tag_removed = await h.toasts(1000);
  rec.tags_after_remove = tags(app);
  await h.shot('sc-tags-available');
  await h.page.locator('#available-tags-section button:visible', { hasText: TAG_A }).first().click();
  rec.tag_added = await h.toasts(1000);
  rec.tags_after_add = tags(app);

  // Webhooks: the empty state of a Docker image application, then a Git based one.
  await h.go(path(app, 'webhooks'));
  await h.page.waitForTimeout(500);
  await h.shot('sc-webhooks-image');
  psql(`update applications set build_pack = 'nixpacks' where uuid = '${app}'`);
  await h.go(path(app, 'webhooks'));
  await h.page.waitForTimeout(500);
  await h.shot('sc-webhooks');
  rec.webhook_inputs = (await h.page.locator('input[readonly]:visible').evaluateAll((els) => els.map((e) => e.value))).map((v) => v.replace(app, 'APP'));
  start = auditMax();
  await h.page.locator('button[aria-label="Generate GitHub webhook secret"]').click();
  rec.generate_toast = await h.toasts(1000);
  rec.generate_row = app_row(app);
  rec.generate_audit = audit(start, name);
  const secret = h.page.locator('label:visible', { hasText: 'Webhook secret' }).nth(1).locator('xpath=ancestor::div[.//input][1]//input').first();
  await secret.fill('short');
  await save(h);
  rec.secret_bad = await h.toasts(800);
  await h.shot('sc-webhook-bad');
  await secret.fill('0123456789abcdef');
  await save(h);
  rec.secret_toast = await h.toasts(800);
  psql(`update applications set build_pack = 'dockerimage' where uuid = '${app}'`);
  return rec;
}, { before: dropTags, after: dropTags });
