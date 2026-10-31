// Application Domains and Maintenance: the same steps on both UIs, each on its own application.
// Compares toasts, field errors and the rows, with the application uuid taken out.
//   node compat/ui/flow_domains.mjs <port>
import { flow, psql, tokens } from './flow.mjs';

const [port] = process.argv.slice(2);
const SERVER = 'hml20fsj5eles0xfiqidq8j5';
const REF = 'http://127.0.0.1:18000';
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
const P = (await api('POST', '/projects', { name: `dom-${stamp}` })).uuid;
const E = (await api('GET', `/projects/${P}/environments`))[0].uuid;
const apps = [];
const newApp = async (name, domains) => {
  const body = { project_uuid: P, server_uuid: SERVER, environment_name: 'production', docker_registry_image_name: 'nginx', docker_registry_image_tag: 'alpine', ports_exposes: '80', name, instant_deploy: false };
  if (domains) body.domains = domains;
  const { uuid } = await api('POST', '/applications/dockerimage', body);
  apps.push(uuid);
  return uuid;
};
const other = await newApp('dom-other', `https://taken-${stamp}.example.test`);

const rows = (uuid) => {
  const list = JSON.parse(psql(`select coalesce(json_agg(r), '[]') from (select a.fqdn, a.domain_port_overrides, a.noindex_domains, a.redirect, a.custom_labels, s.is_force_https_enabled, s.is_permanent_redirect_enabled from applications a join application_settings s on s.application_id = a.id where a.uuid = '${uuid}') r`));
  for (const r of list) if (r.custom_labels) r.custom_labels = Buffer.from(r.custom_labels, 'base64').toString();
  return JSON.parse(JSON.stringify(list).replaceAll(uuid, 'APP'));
};
const click = (h, re) => h.page.locator('button:visible', { hasText: re }).last().evaluate((b) => b.click());
const fillDomain = async (h, prefix, host, port = '') => {
  await h.page.locator(`#${prefix}-host`).fill(host);
  await h.page.locator(`#${prefix}-port`).fill(port);
};
const table = async (h) => (await h.page.locator('.data-table-row.service-domains-overview-grid').allInnerTexts()).map((t) => t.replace(/\s+/g, ' ').trim());
const rowFor = (h, host) => h.page.locator('.data-table-row.service-domains-overview-grid', { hasText: host }).first();

try {
  await flow(port, 'alice', async (h) => {
    const rec = {};
    h.page.setDefaultTimeout(10000);
    const app = await newApp(`dom-${h.label}`);
    const page = `/project/${P}/environment/${E}/application/${app}/domains`;
    const dom = `app-${stamp}.example.test`;
    await h.go(page);
    await h.page.waitForTimeout(1500);
    await h.shot('dom-empty');
    rec.empty = await h.page.locator('text=No domains configured').count();

    // Add: validation, unknown port, then saved with its www pair.
    await click(h, /^\s*Add domain\s*$/);
    await h.page.waitForTimeout(400);
    await h.shot('dom-add', false);
    await fillDomain(h, 'newDomainParts', 'bad host');
    await click(h, /^\s*Save\s*$/);
    await h.page.waitForTimeout(1000);
    rec.add_bad = await h.page.locator('.text-red-500:visible, .text-error:visible').evaluateAll((els) => els.map((e) => (e.textContent ?? '').trim()).filter((x) => x.includes('omain')));
    await fillDomain(h, 'newDomainParts', dom, '8123');
    await click(h, /^\s*Save\s*$/);
    await h.page.waitForTimeout(1200);
    await h.shot('dom-port-warning', false);
    rec.port_warning = await h.page.locator('text=Unrecognized internal port').count();
    await click(h, /^\s*Use this port anyway\s*$/);
    rec.add_toast = await h.toasts(2500);
    await h.page.waitForTimeout(800);
    rec.after_add = rows(app);
    rec.table_add = await table(h);
    await h.shot('dom-rows');

    // Conflict with another application.
    await click(h, /^\s*Add domain\s*$/);
    await h.page.waitForTimeout(400);
    await fillDomain(h, 'newDomainParts', `taken-${stamp}.example.test`);
    await click(h, /^\s*Save\s*$/);
    await h.page.waitForTimeout(1500);
    await h.shot('dom-conflict', false);
    rec.conflict = await h.page.locator(`text=taken-${stamp}.example.test`).count().then((n) => n > 0);
    await h.page.keyboard.press('Escape');
    await h.page.waitForTimeout(500);
    await h.go(page);
    await h.page.waitForTimeout(1200);

    // Settings dialog: noindex, redirect.
    await h.shot('dom-before-edit');
    await rowFor(h, `https://${dom}`).locator('.service-domain-actions button').nth(1).evaluate((b) => b.click(), null, { timeout: 8000 });
    await h.page.waitForTimeout(500);
    await h.shot('dom-edit', false);
    rec.edit_host = await h.page.locator('#editingDomainParts-host').inputValue();
    rec.edit_port = await h.page.locator('#editingDomainParts-port').inputValue();
    await h.listbox('Search engine indexing', 'Noindex');
    await h.page.waitForTimeout(200);
    await h.listbox('www redirect', 'Redirect to www');
    await h.page.waitForTimeout(200);
    await click(h, /^\s*Save\s*$/);
    rec.edit_toast = await h.toasts(2500);
    await h.page.waitForTimeout(600);
    rec.after_edit = rows(app);
    rec.table_edit = await table(h);

    // Redirects section.
    await h.go(page);
    await h.page.waitForTimeout(1200);
    await h.shot('dom-redirects');
    await h.page.locator('#domain-redirects-section button.listbox-trigger').first().evaluate((b) => b.click());
    await h.page.waitForTimeout(200);
    await h.page.locator('.listbox-option:visible', { hasText: 'Disabled' }).first().evaluate((b) => b.click());
    rec.https_toast = await h.toasts(2000);
    await h.page.locator('#domain-redirects-section button.listbox-trigger').last().evaluate((b) => b.click());
    await h.page.waitForTimeout(200);
    await h.page.locator('.listbox-option:visible', { hasText: 'Permanent' }).first().evaluate((b) => b.click());
    rec.perm_toast = await h.toasts(2000);
    rec.after_settings = rows(app);

    // Remove.
    await h.go(page);
    await h.page.waitForTimeout(1200);
    await rowFor(h, `https://www.${dom}`).locator('.service-domain-actions button').nth(2).evaluate((b) => b.click());
    await h.page.waitForTimeout(500);
    await h.shot('dom-remove', false);
    for (let i = 0; i < 3; i++) {
      await h.page.locator('button:visible', { hasText: /^\s*(Continue|Remove domain|Confirm)\s*$/ }).last().evaluate((b) => b.click()).catch(() => {});
      await h.page.waitForTimeout(700);
    }
    rec.remove_toast = await h.toasts(2000);
    rec.after_remove = rows(app);

    // Generate: the sslip name of the server.
    await h.go(page);
    await h.page.waitForTimeout(1200);
    await click(h, /^\s*Add domain\s*$/);
    await h.page.waitForTimeout(400);
    await click(h, /^\s*Generate domain\s*$/);
    rec.generate_toast = await h.toasts(2500);
    rec.after_generate = rows(app);

    // Maintenance on the application page.
    await h.go(page);
    await h.page.waitForTimeout(1500);
    rec.maint = await h.page.locator('#maintenance-section').innerText().then((t) => t.replace(/\s+/g, ' ')).catch(() => null);
    await h.shot('dom-final');

    // Free the domains for the other side.
    psql(`update applications set fqdn = null, domain_port_overrides = null where uuid = '${app}'`);
    return rec;
  });
} finally {
  for (const a of apps) await api('DELETE', `/applications/${a}`).catch(() => {});
  await new Promise((r) => setTimeout(r, 12000));
  await api('DELETE', `/projects/${P}`).catch((e) => console.log('leftover project', P, e.message));
}
void other;
