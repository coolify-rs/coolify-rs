// Advanced, Rollback and Metrics sections: same steps on both UIs, each on its own application;
// compares toasts, rows and audit_events.
//   node compat/ui/flow_shared_advanced.mjs <port> <refAppUuid> <rsAppUuid>
import { flow, psql, auditMax, auditSince } from './flow.mjs';

const [port, refApp, rsApp] = process.argv.slice(2);
const path = (app, s) => `/project/j8xm2emlllhfnyebjzgyzp7z/environment/uj3st9yvtvzxh7rul3c3opcz/application/${app}/${s}`;
const COLS = 'disable_build_cache, inject_build_args_to_dockerfile, include_source_commit_in_build, is_consistent_container_name_enabled, custom_internal_name, custom_container_name_prefix, stop_grace_period, rolling_update_drain_seconds, is_log_drain_enabled, is_gpu_enabled, gpu_driver, gpu_count, gpu_device_ids, is_gzip_enabled, is_stripprefix_enabled, traefik_certificate_resolver, docker_images_to_keep';
const row = (app) => JSON.parse(psql(`select row_to_json(r) from (select ${COLS}, (select max_restart_count from applications a where a.id = s.application_id) as max_restart_count from application_settings s where application_id = (select id from applications where uuid = '${app}')) r`));
const audit = (start, name) =>
  auditSince(start, '^ui\\.application\\.').map((e) => {
    const m = e.metadata ?? {};
    for (const k of ['application_uuid', 'application_name']) delete m[k];
    return { event: e.event, description: e.description.replace(name, 'APP'), metadata: m };
  });
const field = (h, label) => h.page.locator('label:visible', { hasText: label }).first().locator('xpath=ancestor::div[.//input][1]//input').first();

await flow(port, 'alice', async (h) => {
  const app = h.label === 'ref' ? refApp : rsApp;
  const name = h.label === 'ref' ? 'shared-ref' : 'shared-rs';
  const rec = {};
  psql(`update application_settings set disable_build_cache = false, inject_build_args_to_dockerfile = true, include_source_commit_in_build = false, is_consistent_container_name_enabled = false, custom_internal_name = null, custom_container_name_prefix = null, stop_grace_period = null, rolling_update_drain_seconds = 5, is_gpu_enabled = false, is_gzip_enabled = true, is_stripprefix_enabled = true, traefik_certificate_resolver = null, docker_images_to_keep = 2 where application_id = (select id from applications where uuid = '${app}')`);
  await h.go(path(app, 'advanced'));
  await h.page.waitForTimeout(600);
  await h.shot('sa-advanced');
  let start = auditMax();
  await h.listbox('Build cache', 'Rebuild from scratch');
  rec.cache_toast = await h.toasts(900);
  await h.listbox('Gzip compression', 'Disabled');
  rec.gzip_toast = await h.toasts(1000);
  rec.after_listbox = row(app);
  rec.listbox_audit = audit(start, name);

  await field(h, 'Container name prefix').fill('My Prefix');
  await h.page.waitForTimeout(200);
  await field(h, 'Container name prefix').press('Enter');
  rec.prefix_toast = await h.toasts(1000);
  rec.prefix_value = await field(h, 'Container name prefix').inputValue();

  await h.listbox('Container naming', 'Consistent name');
  await h.page.waitForTimeout(800);
  await h.shot('sa-consistent');
  await field(h, 'Custom container name').fill('Custom Name');
  await field(h, 'Custom container name').press('Enter');
  rec.custom_toast = await h.toasts(1000);
  rec.custom_row = row(app);

  const stop = field(h, 'Stop grace period');
  await stop.fill('99999');
  await stop.blur();
  rec.stop_bad = await h.toasts(1000);
  await h.page.waitForTimeout(200);
  await h.shot('sa-stop-bad');
  await stop.fill('120');
  await stop.blur();
  rec.stop_toast = await h.toasts(1000);
  const drain = field(h, 'Rolling update drain');
  await drain.fill('10');
  await drain.blur();
  rec.drain_toast = await h.toasts(1000);
  const restart = field(h, 'Max restart count');
  await restart.fill('3');
  await restart.blur();
  rec.restart_toast = await h.toasts(1000);
  rec.numbers_row = row(app);

  await h.listbox('Log drain', 'Send logs');
  rec.drain_error = await h.toasts(1000);

  await h.listbox('GPU access', 'Enabled');
  await h.page.waitForTimeout(800);
  rec.gpu_on = await h.toasts(600);
  await h.shot('sa-gpu');
  await field(h, 'GPU count').fill('abc');
  await field(h, 'GPU count').press('Enter');
  rec.gpu_bad = await h.toasts(800);
  await field(h, 'GPU count').fill('all');
  await field(h, 'GPU count').press('Enter');
  rec.gpu_toast = await h.toasts(800);
  rec.gpu_row = row(app);

  await h.listbox('Certificate resolver', 'Custom resolver').catch(() => {});
  await h.page.waitForTimeout(500);
  rec.has_resolver_name = await field(h, 'Resolver name').count();

  // Rollback settings.
  await h.go(path(app, 'rollback'));
  await h.page.waitForTimeout(1200);
  await h.shot('sa-rollback');
  await field(h, 'Images to keep').fill('5');
  await field(h, 'Images to keep').press('Enter');
  rec.keep_toast = await h.toasts(900);
  rec.keep_row = row(app);

  // Metrics.
  await h.go(path(app, 'metrics'));
  await h.page.waitForTimeout(800);
  await h.shot('sa-metrics');
  rec.metrics_text = (await h.page.locator('#metrics-overview-section').first().innerText()).replace(/\s+/g, ' ');
  // The names must be free for the other side's application on the same server.
  psql(`update application_settings set custom_internal_name = null, custom_container_name_prefix = null where application_id = (select id from applications where uuid = '${app}')`);
  return rec;
});
