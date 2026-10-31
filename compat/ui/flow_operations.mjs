// Resource operations, clone-me and the Danger page: same steps on both UIs on one fixture project.
// Compares redirects, the rows of the clones, moved rows and files on the test VM.
//   node compat/ui/flow_operations.mjs <port>
// Everything created is tracked by uuid and removed at the end, also after a failure.
import { execFileSync } from 'node:child_process';
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
const APP = String.raw`App\Models\Application`;
const SVC = String.raw`App\Models\Service`;
const PG = String.raw`App\Models\StandalonePostgresql`;

// --- fixture -----------------------------------------------------------------------------------
const stamp = Date.now().toString(36);
const project = await api('POST', '/projects', { name: `ops-${stamp}` });
const P = project.uuid;
const envs = await api('GET', `/projects/${P}/environments`);
const E = envs[0].uuid;
const two = await api('POST', `/projects/${P}/environments`, { name: 'ops-two' });
const app = (await api('POST', '/applications/dockerimage', { project_uuid: P, server_uuid: SERVER, environment_name: 'production', docker_registry_image_name: 'nginx', docker_registry_image_tag: 'alpine', ports_exposes: '80', name: 'ops-app', instant_deploy: false })).uuid;
const appId = psql(`select id from applications where uuid = '${app}'`);
await api('POST', `/applications/${app}/envs`, { key: 'OPS_VAR', value: 'one' });
const db = (await api('POST', '/databases/postgresql', { project_uuid: P, server_uuid: SERVER, environment_name: 'production', name: 'ops-db', instant_deploy: false })).uuid;
const compose = Buffer.from('services:\n  web:\n    image: nginx:alpine\n    volumes:\n      - web-data:/data\n    environment:\n      - SERVICE_FQDN_WEB\nvolumes:\n  web-data:\n').toString('base64');
const service = (await api('POST', '/services', { project_uuid: P, server_uuid: SERVER, environment_name: 'production', name: 'ops-svc', docker_compose_raw: compose, instant_deploy: false })).uuid;
// A volume and a file storage with content: what used to block the Danger page.
psql(`insert into local_persistent_volumes (uuid, name, mount_path, resource_type, resource_id, created_at, updated_at) values ('opsv${stamp}', '${app}-vol', '/vol', '${APP}', ${appId}, now(), now())`);
psql(`insert into local_file_volumes (uuid, fs_path, mount_path, content, is_directory, resource_type, resource_id, created_at, updated_at) values ('opsf${stamp}', '/data/coolify/applications/${app}/conf.txt', '/etc/conf.txt', 'hello', false, '${APP}', ${appId}, now(), now())`);

const created = { apps: [app], dbs: [db], services: [service], projects: [P], envs: [] };

const U = /(?<![a-z0-9])[a-z0-9]{24}(?![a-z0-9])/g;
const TIME = /\d{4}-\d\d-\d\d[T ]\d\d:\d\d:\d\d(\.\d+)?Z?/g;
const norm = (v) => JSON.parse(JSON.stringify(v).replace(TIME, 'T').replace(U, 'U'));
const rows = (table, where, drop = []) => {
  const q = `select coalesce(json_agg(r), '[]') from (select * from ${table} where ${where} order by id) r`;
  const list = JSON.parse(psql(q));
  for (const r of list) if (r.custom_labels) r.custom_labels = Buffer.from(r.custom_labels, 'base64').toString();
  return norm(list.map((r) => { for (const k of ['id', 'created_at', 'updated_at', 'application_id', 'resource_id', 'resourceable_id', 'database_id', 'service_id', 'environment_id', 'destination_id', ...drop]) delete r[k]; return r; }));
};
const idOf = (table, uuid) => psql(`select id from ${table} where uuid = '${uuid}'`);
const appRows = (uuid) => {
  const id = idOf('applications', uuid);
  return {
    app: rows('applications', `id = ${id}`),
    settings: rows('application_settings', `application_id = ${id}`),
    volumes: rows('local_persistent_volumes', `resource_type = '${APP}' and resource_id = ${id}`),
    files: rows('local_file_volumes', `resource_type = '${APP}' and resource_id = ${id}`),
    envs: rows('environment_variables', `resourceable_type = '${APP}' and resourceable_id = ${id}`),
  };
};
const dbRows = (uuid) => {
  const id = idOf('standalone_postgresqls', uuid);
  return {
    db: rows('standalone_postgresqls', `id = ${id}`),
    volumes: rows('local_persistent_volumes', `resource_type = '${PG}' and resource_id = ${id}`),
    envs: rows('environment_variables', `resourceable_type = '${PG}' and resourceable_id = ${id}`, ['value']),
  };
};
const svcRows = (uuid) => {
  const id = idOf('services', uuid);
  const sub = psql(`select id from service_applications where service_id = ${id}`);
  return {
    service: rows('services', `id = ${id}`, ['docker_compose']),
    apps: rows('service_applications', `service_id = ${id}`),
    envs: rows('environment_variables', `resourceable_type = '${SVC}' and resourceable_id = ${id}`, ['value']),
    volumes: rows('local_persistent_volumes', `resource_type = 'App\\Models\\ServiceApplication' and resource_id = ${sub}`),
  };
};
const envOf = (table, uuid) => psql(`select e.name from ${table} t join environments e on e.id = t.environment_id where t.uuid = '${uuid}'`);
const sect = (seg, uuid, s) => `/project/${P}/environment/${E}/${seg}/${uuid}/${s}`;
const uuidIn = (url, seg) => new RegExp(`/${seg}/([a-z0-9]+)`).exec(url)?.[1];

const clickText = async (h, re) => h.page.locator('button:visible', { hasText: re }).last().click({ force: true });
const pathOf = (url) => new URL(url).pathname;
const normPath = (p) => p.replace(U, 'U');

async function waitUrl(h, re) {
  await h.page.waitForFunction((s) => new RegExp(s).test(location.pathname), re.source, { timeout: 20000 }).catch(() => {});
  await h.page.waitForTimeout(1200);
}

const confirmAll = async (h, text) => {
  for (let i = 0; i < 5; i++) {
    const pwd = h.page.locator('input[type=password]:visible').last();
    if (await pwd.count()) await pwd.fill(h.secret).catch(() => {});
    const typed = h.page.locator('input[type=text]:visible:not([readonly]):not([disabled])').last();
    if (await typed.count()) await typed.fill(text).catch(() => {});
    const go = h.page.locator('button:visible', { hasText: /^\s*(Continue|Confirm|Permanently Delete|Delete)\s*$/ }).last();
    await go.click({ timeout: 1500 }).catch(() => {});
    await h.page.waitForTimeout(900);
    if (!(await h.page.locator('button:visible', { hasText: /^\s*Cancel\s*$/ }).count())) break;
  }
};

async function fixtureDelete() {
  for (const a of created.apps) await api('DELETE', `/applications/${a}`).catch(() => {});
  for (const d of created.dbs) await api('DELETE', `/databases/${d}`).catch(() => {});
  for (const s of created.services) await api('DELETE', `/services/${s}`).catch(() => {});
  await new Promise((r) => setTimeout(r, 15000));
  for (const e of created.envs) await api('DELETE', `/projects/${e.project}/environments/${e.env}`).catch(() => {});
  for (const p of created.projects) await api('DELETE', `/projects/${p}`).catch((e) => console.log('leftover project', p, e.message));
}

try {
  await flow(port, 'alice', async (h) => {
    const rec = {};
    const label = h.label;
    // Screens.
    for (const [name, seg, uuid] of [['app', 'application', app], ['db', 'database', db], ['svc', 'service', service]]) {
      await h.go(sect(seg, uuid, 'resource-operations'));
      await h.page.waitForTimeout(1500);
      await h.shot(`ops-${name}`);
    }
    await h.go(`/project/${P}/environment/${E}/clone`);
    await h.page.waitForTimeout(1200);
    await h.shot('ops-cloneme');
    await h.go(sect('service', service, 'danger'));
    await h.page.waitForTimeout(1200);
    await h.shot('ops-svc-danger');

    // Clone with the defaults.
    for (const [name, seg, uuid, table, fn] of [
      ['application', 'application', app, 'applications', appRows],
      ['database', 'database', db, 'standalone_postgresqls', dbRows],
      ['service', 'service', service, 'services', svcRows],
    ]) {
      await h.go(sect(seg, uuid, 'resource-operations'));
      await h.page.waitForTimeout(1500);
      await clickText(h, /^\s*Clone resource\s*$/);
      await waitUrl(h, new RegExp(`/${seg}/(?!${uuid})[a-z0-9]+`));
      const next = uuidIn(h.page.url(), seg);
      rec[`clone_${name}_url`] = normPath(pathOf(h.page.url()));
      if (!next || next === uuid) { rec[`clone_${name}_rows`] = 'no redirect'; continue; }
      if (seg === 'application') created.apps.push(next);
      if (seg === 'database') created.dbs.push(next);
      if (seg === 'service') created.services.push(next);
      rec[`clone_${name}_rows`] = fn(next);
      if (seg === 'application') {
        await h.page.waitForTimeout(4000);
        rec.clone_file = vm(`cat /data/coolify/applications/${app}/conf.txt 2>&1 || true; ls /data/coolify/applications/${next} 2>&1 | head -5`);
      }
      // Move the clone to the other environment.
      await h.go(sect(seg, next, 'resource-operations'));
      await h.page.waitForTimeout(1500);
      await h.shot(`ops-${seg}-clone`);
      await h.listbox('Project', `ops-${stamp}`).catch(() => {});
      await h.page.locator('#move-resource-section button.listbox-trigger').first().click().catch(() => {});
      await h.page.waitForTimeout(200);
      await h.page.locator('.listbox-option:visible', { hasText: `ops-${stamp}` }).first().click().catch(() => {});
      await h.page.waitForTimeout(300);
      await h.page.locator('#move-resource-section button.listbox-trigger').nth(1).click().catch(() => {});
      await h.page.waitForTimeout(200);
      await h.page.locator('.listbox-option:visible', { hasText: 'ops-two' }).first().click().catch(() => {});
      await h.page.waitForTimeout(300);
      await h.shot(`ops-${seg}-move`);
      await clickText(h, /^\s*Move resource\s*$/);
      await waitUrl(h, new RegExp(`/environment/${two.uuid}/`));
      rec[`move_${name}_url`] = normPath(pathOf(h.page.url()));
      rec[`move_${name}_env`] = envOf(table, next);
    }

    // CloneMe to an environment.
    await h.go(`/project/${P}/environment/${E}/clone`);
    await h.page.waitForTimeout(1200);
    const newEnv = `ops-env-${label}`;
    await h.field('New name').fill(newEnv);
    await h.page.locator('button.clone-destinations-table-grid:visible').first().click();
    await h.page.waitForTimeout(400);
    await h.shot('ops-cloneme-selected');
    await clickText(h, /^\s*Clone to environment\s*$/);
    await waitUrl(h, /\/environment\/(?!.*clone)[a-z0-9]+$/);
    rec.cloneme_url = normPath(pathOf(h.page.url()));
    const target = pathOf(h.page.url()).split('/').pop();
    created.envs.push({ project: P, env: newEnv });
    const envId = psql(`select id from environments where uuid = '${target}'`);
    if (envId) {
      for (const [t, l] of [['applications', 'apps'], ['standalone_postgresqls', 'dbs'], ['services', 'services']]) {
        const found = psql(`select coalesce(string_agg(uuid, ','), '') from ${t} where environment_id = ${envId}`).split(',').filter(Boolean);
        created[l].push(...found);
        rec[`cloneme_${t}`] = found.length;
        if (t === 'applications' && found[0]) rec.cloneme_app_rows = appRows(found[0]);
        if (t === 'standalone_postgresqls' && found[0]) rec.cloneme_db_rows = dbRows(found[0]);
        if (t === 'services' && found[0]) rec.cloneme_svc_rows = svcRows(found[0]);
      }
    }
    // Errors: the same name again.
    await h.go(`/project/${P}/environment/${E}/clone`);
    await h.page.waitForTimeout(1000);
    await h.field('New name').fill(newEnv);
    await h.page.locator('button.clone-destinations-table-grid:visible').first().click();
    await h.page.waitForTimeout(400);
    await clickText(h, /^\s*Clone to environment\s*$/);
    rec.cloneme_taken = await h.toasts(1500);
    await h.field('New name').fill('');
    await clickText(h, /^\s*Clone to environment\s*$/);
    await h.page.waitForTimeout(800);
    await h.shot('ops-cloneme-empty');
    rec.cloneme_empty_text = await h.page.locator('text=Please enter a name').count();
    return rec;
  });
} finally {
  await fixtureDelete();
}
