// `Server\Delete` in the UI: the dialog with its checkboxes, and "Delete all resources" for real on both sides.
// Then on coolify-rs only, Hostinger through a local fake (the reference's provider hosts are fixed).
//   COOLIFY_RS_CLOUD_API_BASE=http://127.0.0.1:18188 scripts/dev-server.sh 18088
//   node compat/ui/flow_server_delete.mjs 18088
import http from 'node:http';
import { flow, psql, tokens } from './flow.mjs';

const [port] = process.argv.slice(2);
const secret = (await import('node:fs')).readFileSync(new URL('../reference/secrets/ui-login.txt', import.meta.url), 'utf8').match(/^password:\s*(.+)$/m)[1].trim();
const REF = 'http://127.0.0.1:18000';
const KEY = 'hv41morfjv9x0av5kakzdmv8';
const clean = (s) => s.replace(/\s+/g, ' ').trim();

async function api(method, path, body, base = REF) {
  const r = await fetch(base + path, {
    method,
    headers: { Authorization: `Bearer ${tokens.acme_owner_root}`, Accept: 'application/json', 'Content-Type': 'application/json' },
    body: body ? JSON.stringify(body) : undefined,
  });
  return r.json();
}
let serial = 60;
const made = { servers: [], projects: [], tokens: [] };
async function server(name, extraSql = '') {
  const s = await api('POST', '/api/v1/servers', { name, ip: `192.0.2.${serial++}`, port: 22, user: 'root', private_key_uuid: KEY, instant_validate: false });
  const id = psql(`select id from servers where uuid='${s.uuid}'`);
  made.servers.push(id);
  if (extraSql) psql(`update servers set ${extraSql} where id=${id}`);
  return { uuid: s.uuid, id };
}
async function confirm(page) {
  await page.locator('button:visible', { hasText: 'Delete server' }).last().click();
  await page.waitForTimeout(800);
}

async function walk(h, name) {
  for (let step = 0; step < 4 && (await h.page.locator('.fixed button:visible', { hasText: /Continue|Confirm|Permanently/ }).count()); step++) {
    const text = h.page.locator('.fixed input[type=text]:not([readonly]):not([disabled]):visible');
    if (await text.count()) await text.first().fill(name);
    const pw = h.page.locator('.fixed input[type=password]:visible');
    if (await pw.count()) await pw.first().fill(secret);
    await h.page.waitForTimeout(200);
    await h.page.locator('.fixed button:visible', { hasText: /Continue|Confirm|Permanently/ }).last().click();
    await h.page.waitForTimeout(700);
  }
}

const seen = [];
const fake = http.createServer((req, res) => {
  seen.push(`${req.method} ${req.url}`);
  res.setHeader('Content-Type', 'application/json');
  res.end(req.url.includes('virtual-machines') ? '{"subscription_id":"sub-9"}' : '{}');
});
await new Promise((ok) => fake.listen(18188, '127.0.0.1', ok));

try {
  await flow(port, 'alice', async (h) => {
    const r = {};
    const project = await api('POST', '/api/v1/projects', { name: `s2-ui-del-${h.label}` });
    made.projects.push(project.uuid);
    const env = (await api('GET', `/api/v1/projects/${project.uuid}`)).environments[0].uuid;
    // The dialog with every checkbox. Nothing is submitted here, so no provider is reached.
    const linked = await server(`s2-ui-linked-${h.label}`, "hetzner_server_id=1, vultr_instance_id='v', digitalocean_droplet_id=2, hostinger_virtual_machine_id=3");
    await h.go(`/server/${linked.uuid}/danger`);
    await h.page.waitForTimeout(1200);
    await confirm(h.page);
    r.linkedDialog = clean(await h.page.locator('.fixed:visible', { hasText: 'Confirm Server Deletion' }).last().innerText({ timeout: 5000 }).catch(async (e) => { await h.shot('sd-debug', false); throw e; }));
    await h.shot('sd-linked', false);
    await h.page.keyboard.press('Escape');
    // Force delete with one undeployed application.
    const s = await server(`s2-ui-force-${h.label}`);
    const app = await api('POST', '/api/v1/applications/dockerimage', { project_uuid: project.uuid, server_uuid: s.uuid, environment_uuid: env, instant_deploy: false, name: 's2-ui-app', docker_registry_image_name: 'nginx', ports_exposes: '80' });
    await h.go(`/server/${s.uuid}/danger`);
    await h.page.waitForTimeout(1200);
    await confirm(h.page);
    r.forceDialog = clean(await h.page.locator('.fixed:visible', { hasText: 'Confirm Server Deletion' }).last().innerText({ timeout: 5000 }).catch(async (e) => { await h.shot('sd-debug', false); throw e; })).replaceAll(`s2-ui-force-${h.label}`, 'NAME');
    await h.shot('sd-force', false);
    await h.page.getByLabel(/Delete all resources/).check();
    await walk(h, `s2-ui-force-${h.label}`);
    let gone = false;
    for (let i = 0; i < 30 && !gone; i++) {
      await h.page.waitForTimeout(1000);
      gone = !psql(`select id from servers where id=${s.id}`) && !psql(`select id from applications where uuid='${app.uuid}'`);
    }
    r.gone = gone;
    r.landed = new URL(h.page.url()).pathname;
    r.audit = psql(`select event from audit_events where resource_uuid='${s.uuid}' order by id`);
    r.toasts = await h.page.evaluate(() => window.__toasts);
    r.auditMeta = psql(`select metadata->>'server_name', metadata->>'changed_fields', source, level, description from audit_events where resource_uuid='${s.uuid}' and event='ui.server.deleted'`).replaceAll(`s2-ui-force-${h.label}`, 'NAME');
    if (h.label === 'rs') {
      const t = await api('POST', '/api/v1/cloud-tokens', { provider: 'hostinger', token: 'fake-hostinger', name: 's2-hostinger' }, h.base);
      made.tokens.push(psql(`select id from cloud_provider_tokens where uuid='${t.uuid}'`));
      const hs = await server('s2-ui-hostinger', `hostinger_virtual_machine_id=3, cloud_provider_token_id=${made.tokens[0]}`);
      seen.length = 0;
      await h.go(`/server/${hs.uuid}/danger`);
      await h.page.waitForTimeout(1200);
      await confirm(h.page);
      await h.page.getByLabel(/Also delete VPS from Hostinger/).check();
      await walk(h, 's2-ui-hostinger');
      for (let i = 0; i < 20 && psql(`select id from servers where id=${hs.id}`); i++) await h.page.waitForTimeout(500);
      const ok = !psql(`select id from servers where id=${hs.id}`) && seen.includes('GET /hostinger/api/vps/v1/virtual-machines/3') && seen.includes('DELETE /hostinger/api/billing/v1/subscriptions/sub-9/auto-renewal/disable');
      console.log(ok ? 'ok   hostinger through the fake' : `FAIL hostinger ${JSON.stringify(seen)}`);
    }
    return r;
  });
} finally {
  for (const id of made.servers) {
    psql(`delete from standalone_dockers where server_id=${id}`);
    psql(`delete from server_settings where server_id=${id}`);
    psql(`delete from servers where id=${id}`);
  }
  await new Promise((ok) => setTimeout(ok, 3000));
  for (const t of made.tokens) psql(`delete from cloud_provider_tokens where id=${t}`);
  for (const p of made.projects) console.log('project', (await api('DELETE', `/api/v1/projects/${p}`)).message);
  fake.close();
}
