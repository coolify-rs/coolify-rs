// Preview deployments: the same steps on both UIs, on the fixtures of compat/diff/appui_fixture.py.
// Each side uses its own pull request id; rows are compared with that id taken out. Real
// deployments and stops on the test VM. No step loads pull requests from GitHub.
//   node compat/ui/flow_appui_previews.mjs <port>
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { flow, psql, auditMax, auditSince, tokens } from './flow.mjs';

const [port] = process.argv.slice(2);
const F = JSON.parse(fs.readFileSync(path.join(os.tmpdir(), 'coolify-rs-appui-fixture.json'), 'utf8'));
const base = `/project/${F.project}/environment/${F.environment}`;
const REF = 'http://127.0.0.1:18000';
const stamp = Math.floor(Date.now() / 1000) % 900;
const tracked = [];
const api = async (method, p, body) => {
  for (let i = 0; ; i++) {
    const r = await fetch(`${REF}/api/v1${p}`, {
      method,
      headers: { Authorization: `Bearer ${tokens.acme_owner_root}`, 'Content-Type': 'application/json', Accept: 'application/json' },
      body: body ? JSON.stringify(body) : undefined,
    });
    // The reference's rate limit is shared with other agents.
    if (r.status === 429 && i < 20) {
      await new Promise((d) => setTimeout(d, 5000));
      continue;
    }
    const text = await r.text();
    if (!r.ok && method === 'POST') console.log('api', p, r.status, text.slice(0, 200));
    return text;
  }
};
const scrub = (v, pr, uuid) => JSON.parse(JSON.stringify(v).replaceAll(`pr-${pr}`, 'pr-N').replaceAll(`${pr}.`, 'N.').replaceAll(`"pull_request_id":${pr}`, '"pull_request_id":"N"').replaceAll(uuid, 'APP'));
const row = (uuid, pr) =>
  JSON.parse(
    psql(`select coalesce(json_agg(r), '[]') from (select p.pull_request_id, p.pull_request_html_url, p.fqdn, p.domain_port_overrides, p.docker_compose_domains, p.docker_registry_image_tag, p.git_type, p.status, p.deleted_at is not null as deleted from application_previews p join applications a on a.id = p.application_id where a.uuid = '${uuid}' and p.pull_request_id = ${pr}) r`),
  );
const settings = (uuid) =>
  JSON.parse(psql(`select row_to_json(r) from (select s.is_preview_deployments_enabled, s.is_pr_deployments_public_enabled, a.preview_url_template from application_settings s join applications a on a.id = s.application_id where a.uuid = '${uuid}') r`));
const deployment = (uuid, pr) =>
  JSON.parse(psql(`select coalesce(json_agg(r), '[]') from (select pull_request_id, docker_registry_image_tag, force_rebuild, restart_only, is_webhook, is_api from application_deployment_queues where application_id = (select id::text from applications where uuid='${uuid}') and pull_request_id = ${pr} order by id) r`));
const finished = async (uuid, pr) => {
  for (let i = 0; i < 90; i++) {
    const s = psql(`select status from application_deployment_queues where application_id = (select id::text from applications where uuid='${uuid}') and pull_request_id = ${pr} order by id desc limit 1`);
    if (s === 'finished' || s === 'failed' || s === 'cancelled-by-user') return s;
    await new Promise((r) => setTimeout(r, 2000));
  }
  return 'timeout';
};

try {
await flow(port, 'alice', async (h) => {
  const rec = {};
  h.page.setDefaultTimeout(10000);
  const button = (re) => h.page.locator('button:visible', { hasText: re }).last();
  const click = (re) => button(re).click();
  const side = h.label === 'ref' ? 0 : 1;

  // A Git application: settings and the URL template.
  {
    const uuid = F.git;
    const page = `${base}/application/${uuid}/preview-deployments`;
    const start = auditMax();
    await h.go(page);
    await click(/^\s*Enable preview deployments\s*$/i);
    rec.enableToast = await h.toasts();
    rec.afterEnable = settings(uuid);
    await h.shot('prev-src-enabled');
    await h.listbox('PR deployment access', 'Public');
    rec.publicToast = await h.toasts();
    rec.afterPublic = settings(uuid);
    await h.field('URL template').fill('{{ pr_id }}.preview.{{domain}}');
    await h.page.keyboard.press('Enter');
    rec.templateToast = await h.toasts();
    rec.templateValue = scrub(await h.field('URL template').inputValue(), 0, uuid);
    rec.afterTemplate = scrub(settings(uuid), 0, uuid);
    await click(/^\s*Reset template\s*$/i);
    rec.resetToast = await h.toasts();
    rec.afterReset = settings(uuid);
    await h.field('URL template').fill('');
    await h.page.keyboard.press('Enter');
    rec.emptyTemplateToast = await h.toasts();
    await h.go(page);
    await h.listbox('PR deployment access', 'Repository members');
    await h.toasts();
    await click(/^\s*Disable preview deployments\s*$/i);
    rec.disableToast = await h.toasts();
    rec.audit = auditSince(start, 'settings_updated').map((a) => a.context?.changed_fields ?? a.event);

    // A row created through the API, with a pull request link.
    const pr = 3000 + stamp + side;
    await api('POST', `/applications/${uuid}/previews`, { pull_request_id: pr, pull_request_html_url: `https://example.test/pull/${pr}`, git_type: 'github', instant_deploy: false });
    await h.go(page);
    await click(/^\s*Links\s*$/);
    await h.page.waitForTimeout(250);
    await h.shot('prev-src-links', false);
    rec.srcLinks = await h.page.locator('.listbox-option:visible').allInnerTexts();
    await h.page.keyboard.press('Escape');
    await h.page.mouse.click(900, 5);
    await click(/^\s*Logs\s*$/);
    await h.page.waitForTimeout(250);
    await h.shot('prev-src-logs', false);
    rec.srcLogHrefs = (await h.page.locator('a.listbox-option:visible').evaluateAll((l) => l.map((a) => a.getAttribute('href')))).map((u) => u.replace(/^https?:\/\/[^/]+/, '').replace(`${pr}`, 'N'));
    await h.page.mouse.click(900, 5);
    await click(/^\s*Actions\s*$/);
    await h.page.waitForTimeout(250);
    await h.shot('prev-src-actions', false);
    rec.srcActions = await h.page.locator('.listbox-option:visible').allInnerTexts();
    await h.page.mouse.click(900, 5);
    await h.shot('prev-src-row');
    await api('DELETE', `/applications/${uuid}/previews/${pr}`);
  }

  // A Docker Image application: a real manual preview on the test VM.
  {
    const uuid = F.image;
    const page = `${base}/application/${uuid}/preview-deployments`;
    const pr = 2000 + stamp + side;
    await api('DELETE', `/applications/${uuid}/previews/${pr}`);
    const start = auditMax();
    tracked.push([uuid, pr]);
    await h.go(page);
    // A Preview ID that is not a number breaks upstream's `?int` property (an error page); not compared.
    await h.field('Preview ID').fill('');
    await h.field('Docker tag').fill('');
    await click(/^\s*Deploy preview\s*$/i);
    rec.manualEmptyToast = await h.toasts();
    await h.field('Preview ID').fill(String(pr));
    await h.field('Docker tag').fill('alpine');
    await click(/^\s*Deploy preview\s*$/i);
    await h.page.waitForURL(/\/deployment\//, { timeout: 15000 });
    rec.redirected = true;
    rec.deployStatus = await finished(uuid, pr);
    rec.manualRow = scrub(row(uuid, pr), pr, uuid).map((r) => ({ ...r, status: undefined }));
    rec.manualQueue = scrub(deployment(uuid, pr), pr, uuid);
    await h.go(page);
    await h.page.waitForTimeout(1000);
    await h.shot('prev-image-row');
    rec.domainCount = await h.page.locator('p.truncate', { hasText: /domains?/ }).first().innerText();

    // Docker tag: saved on change.
    await h.page.locator('.application-settings-section-body.is-flush input:not([type=search])').last().fill('stable');
    await h.page.locator('body').click({ position: { x: 900, y: 5 } });
    rec.tagToast = await h.toasts();
    rec.tagRow = row(uuid, pr).map((r) => r.docker_registry_image_tag);

    // Domains: invalid, duplicate, unknown port, edit, generate, remove.
    await click(/^\s*Add domain\s*$/);
    await h.page.waitForTimeout(300);
    await h.shot('prev-domain-add', false);
    await h.page.locator('input[placeholder="app.example.com"]:visible').fill('not a domain!');
    await h.page.locator('button[type=submit]:visible', { hasText: 'Save' }).click();
    await h.page.waitForTimeout(800);
    rec.invalidDomain = (await h.page.locator('input[placeholder="app.example.com"]:visible').locator('xpath=ancestor::div[1]').innerText()).replace(/\s+/g, ' ').trim();
    const host = `pv-${pr}.example.test`;
    await h.page.locator('input[placeholder="app.example.com"]:visible').fill(host);
    await h.page.locator('input[placeholder="3000"]:visible').fill('4567');
    await h.page.locator('button[type=submit]:visible', { hasText: 'Save' }).click();
    await h.page.waitForTimeout(800);
    await h.shot('prev-domain-port', false);
    await click(/^\s*Use this port anyway\s*$/);
    rec.addToast = (await h.toasts()).map((t) => t.replace(' DNS check started.', ''));
    rec.addRow = scrub(row(uuid, pr), pr, uuid).map((r) => ({ fqdn: r.fqdn, overrides: r.domain_port_overrides }));
    await h.shot('prev-domains-two');
    const rowOf = () => h.page.locator('.data-table-row', { hasText: host });
    await rowOf().locator('button[title="Domain settings"]').click();
    await h.page.waitForTimeout(300);
    await h.shot('prev-domain-edit', false);
    rec.editHostValue = scrub(await h.page.locator('input[placeholder="app.example.com"]:visible').inputValue(), pr, uuid);
    rec.editPortValue = await h.page.locator('input[placeholder="3000"]:visible').inputValue();
    await h.page.locator('input[placeholder="app.example.com"]:visible').fill(`edit-${host}`);
    await h.page.locator('input[placeholder="3000"]:visible').fill('');
    await h.page.locator('button[type=submit]:visible', { hasText: 'Save' }).click();
    rec.editToast = (await h.toasts()).map((t) => t.replace(' DNS check started.', '')).filter((t) => !t.includes('DNS is not configured')); // the result of upstream's DNS job, not ported
    rec.editRow = scrub(row(uuid, pr), pr, uuid).map((r) => ({ fqdn: r.fqdn, overrides: r.domain_port_overrides }));
    await h.go(page);
    await h.page.locator('.data-table-row', { hasText: `edit-${host}` }).locator('.service-domain-actions button:visible').last().click();
    await h.page.waitForTimeout(300);
    await h.shot('prev-domain-remove', false);
    await click(/^\s*Remove domain\s*$/);
    rec.removeToast = await h.toasts();
    rec.removeRow = scrub(row(uuid, pr), pr, uuid).map((r) => ({ fqdn: r.fqdn, overrides: r.domain_port_overrides }));
    await h.page.waitForTimeout(500);

    // Stop.
    await h.page.locator('body').click({ position: { x: 900, y: 5 } });
    await click(/^\s*Actions\s*$/);
    await h.page.waitForTimeout(200);
    await h.page.locator('.listbox-option:visible', { hasText: 'Stop' }).click();
    await h.page.waitForTimeout(400);
    await h.shot('prev-stop', false);
    await click(/^\s*Stop preview deployment\s*$/);
    rec.stopToast = await h.toasts(4000);
    rec.stopRow = row(uuid, pr).map((r) => r.status);
    rec.stopActions = null;
    await h.go(page);
    await click(/^\s*Actions\s*$/);
    await h.page.waitForTimeout(200);
    rec.stopActions = await h.page.locator('.listbox-option:visible').allInnerTexts();
    await h.page.mouse.click(900, 5);
    await h.shot('prev-stopped');

    // Delete, with the name to confirm.
    const fqdn = row(uuid, pr)[0].fqdn;
    await click(/^\s*Actions\s*$/);
    await h.page.waitForTimeout(200);
    await h.page.locator('.listbox-option:visible', { hasText: 'Delete' }).click();
    await h.page.waitForTimeout(400);
    await h.shot('prev-delete', false);
    await click(/^\s*Continue\s*$/).catch(() => {});
    await h.page.waitForTimeout(300);
    await h.page.locator('input:visible').last().fill(`${fqdn}/`);
    await button(/^\s*Continue\s*$/).click();
    rec.deleteToast = await h.toasts();
    await h.page.waitForTimeout(800);
    rec.afterDelete = row(uuid, pr).length;
    rec.audit2 = auditSince(start, 'preview_').map((a) => a.event);
  }

  // A Compose application: domains by service.
  {
    const uuid = F.compose;
    const pr = 4000 + stamp + side;
    await api('POST', `/applications/${uuid}/previews`, { pull_request_id: pr, pull_request_html_url: `https://example.test/pull/${pr}`, git_type: 'github', instant_deploy: false });
    const page = `${base}/application/${uuid}/preview-deployments`;
    await h.go(page);
    await click(/^\s*Add domain\s*$/);
    await h.page.waitForTimeout(300);
    await h.shot('prev-compose-add', false);
    await h.page.locator('input[placeholder="app.example.com"]:visible').fill(`w-${pr}.example.test`);
    await h.page.locator('button[type=submit]:visible', { hasText: 'Save' }).click();
    rec.composeAddToast = (await h.toasts()).map((t) => t.replace(' DNS check started.', ''));
    rec.composeRow = scrub(row(uuid, pr), pr, uuid).map((r) => ({ fqdn: r.fqdn, compose: r.docker_compose_domains }));
    await h.shot('prev-compose-row');
    await click(/^\s*Add domain\s*$/);
    await h.page.waitForTimeout(200);
    await click(/^\s*Generate domain\s*$/);
    rec.composeGenerateToast = await h.toasts();
    rec.composeGenerated = scrub(row(uuid, pr), pr, uuid).map((r) => ({ fqdn: r.fqdn, compose: r.docker_compose_domains }));
    await api('DELETE', `/applications/${uuid}/previews/${pr}`);
  }
  return rec;
});
} finally {
  // Whatever a failed run left behind: only the pull requests this run made.
  for (const [uuid, pr] of tracked) await api('DELETE', `/applications/${uuid}/previews/${pr}`);
}
