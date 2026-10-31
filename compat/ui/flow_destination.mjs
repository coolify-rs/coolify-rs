// Additional servers, Refresh GPUs, rollback start, Swarm settings and the member role, on both UIs.
// Uses a second server row of the same team that points at the same test VM, created here.
//   node compat/ui/flow_destination.mjs <port>
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
const stamp = Date.now().toString(36);
const P = (await api('POST', '/projects', { name: `dest-${stamp}` })).uuid;
const E = (await api('GET', `/projects/${P}/environments`))[0].uuid;
const apps = [];
const newApp = async (name) => {
  const { uuid } = await api('POST', '/applications/dockerimage', { project_uuid: P, server_uuid: SERVER, environment_name: 'production', docker_registry_image_name: `opsimg${stamp}`, docker_registry_image_tag: 'abcdef1234', ports_exposes: '80', name, instant_deploy: false });
  apps.push(uuid);
  return uuid;
};
// The second server and its destination.
const srcId = psql(`select id from servers where uuid = '${SERVER}'`);
const cols = (table, skip) => JSON.parse(psql(`select json_agg(column_name::text) from information_schema.columns where table_schema = current_schema() and table_name = '${table}'`)).filter((c) => !skip.includes(c)).map((c) => `"${c}"`).join(', ');
const srvUuid = `opssrv${stamp}`.padEnd(24, '0');
const srvCols = cols('servers', ['id', 'uuid', 'name', 'created_at', 'updated_at']);
const srv2 = psql(`insert into servers (uuid, name, ${srvCols}, created_at, updated_at) select '${srvUuid}', 'ops-second-${stamp}', ${srvCols}, now(), now() from servers where id = ${srcId} returning id`).split('\n')[0];
const setCols = cols('server_settings', ['id', 'server_id', 'created_at', 'updated_at']);
psql(`insert into server_settings (server_id, ${setCols}, created_at, updated_at) select ${srv2}, ${setCols}, now(), now() from server_settings where server_id = ${srcId}`);
const net2 = psql(`insert into standalone_dockers (uuid, name, network, server_id, created_at, updated_at) values ('opsnet${stamp}', 'ops-net', 'coolify', ${srv2}, now(), now()) returning id`).split('\n')[0];
vm(`docker image inspect nginx:alpine >/dev/null 2>&1 || docker pull -q nginx:alpine >/dev/null; docker tag nginx:alpine opsimg${stamp}:abcdef1234; docker tag nginx:alpine opsimg${stamp}:abcdef5678`);

const dests = (uuid) => JSON.parse(psql(`select coalesce(json_agg(r), '[]') from (select (d.server_id = ${srv2}) as second_server, (d.standalone_docker_id = ${net2}) as second_net, d.status from additional_destinations d join applications a on a.id = d.application_id where a.uuid = '${uuid}' order by d.id) r`));
const primary = (uuid) => psql(`select destination_id = ${net2} from applications where uuid = '${uuid}'`);
const sect = (uuid, s) => `/project/${P}/environment/${E}/application/${uuid}/${s}`;

try {
  await flow(port, 'alice', async (h) => {
    const rec = {};
    h.page.setDefaultTimeout(10000);
    const app = await newApp(`dest-${h.label}`);
    const clickBtn = (re) => h.page.locator('button:visible', { hasText: re }).last().evaluate((b) => b.click());

    // Servers: add, make primary, remove.
    await h.go(sect(app, 'servers'));
    await h.page.waitForTimeout(1500);
    await h.shot('multi-available');
    rec.available = await h.page.locator(`text=ops-second-${stamp}`).count();
    await clickBtn(/^\s*Add server\s*$/);
    await h.page.waitForTimeout(1500);
    rec.after_add = dests(app);
    await h.go(sect(app, 'servers'));
    await h.page.waitForTimeout(1500);
    await h.shot('multi-attached');
    rec.sections = (await h.page.locator('.application-settings-section:visible').allInnerTexts()).map((t) => t.replace(/\s+/g, ' ').replaceAll(stamp, 'S'));
    await h.page.locator('#additional-servers-section button', { hasText: /^\s*Actions\s*$/ }).first().evaluate((b) => b.click());
    await h.page.waitForTimeout(300);
    await h.shot('multi-menu', false);
    await h.page.locator('#additional-servers-section .listbox-option', { hasText: 'Make primary' }).first().evaluate((b) => b.click());
    await h.page.waitForTimeout(1500);
    rec.after_promote = { primary: primary(app), dests: dests(app) };
    // Back, so that the second server is the additional one again.
    await h.go(sect(app, 'servers'));
    await h.page.waitForTimeout(1500);
    await h.page.locator('#additional-servers-section button', { hasText: /^\s*Actions\s*$/ }).first().evaluate((b) => b.click());
    await h.page.waitForTimeout(300);
    await h.page.locator('#additional-servers-section .listbox-option', { hasText: 'Make primary' }).first().evaluate((b) => b.click());
    await h.page.waitForTimeout(1500);
    rec.after_back = { primary: primary(app), dests: dests(app) };

    // Refresh GPUs: on the second server row only, the first keeps its metadata.
    psql(`delete from additional_destinations where application_id = (select id from applications where uuid = '${app}')`);
    psql(`update applications set destination_id = ${net2} where uuid = '${app}'`);
    await h.go(sect(app, 'advanced'));
    await h.page.waitForTimeout(1500);
    await clickBtn(/^\s*Refresh GPUs\s*$/);
    rec.gpu_toast = await h.toasts(5000);
    rec.gpu_metadata = psql(`select coalesce(string_agg(k, ',' order by k), '') from json_object_keys((select server_metadata from servers where id = ${srv2})) k`);
    await h.page.waitForTimeout(600);
    await h.shot('gpu-refreshed');
    rec.gpu_text = await h.page.locator('#advanced-gpu-section').innerText().then((t) => t.replace(/\s+/g, ' ').replace(/checked [^.]*? ago/, 'checked T').replaceAll(stamp, 'S')).catch(() => null);
    psql(`update applications set destination_id = (select id from standalone_dockers where server_id = ${srcId} and network = 'coolify' order by id limit 1) where uuid = '${app}'`);

    // Rollback: the queue row.
    await h.go(sect(app, 'rollback'));
    await h.page.waitForTimeout(2500);
    await h.shot('rollback-list');
    rec.rollback_rows = (await h.page.locator('code:visible').allInnerTexts()).filter((t) => t.startsWith('abcdef')).sort();
    const appId = psql(`select id from applications where uuid = '${app}'`);
    await h.page.locator('button:visible', { hasText: /^\s*Roll back to this image\s*$/ }).first().evaluate((b) => b.click());
    await h.page.waitForTimeout(2500);
    rec.rollback_url = new URL(h.page.url()).pathname.replace(app, 'APP').replace(/deployment\/[a-z0-9]+/, 'deployment/D').replace(P, 'P').replace(E, 'E');
    rec.rollback_row = psql(`select rollback, commit like 'abcdef%', force_rebuild, is_api from application_deployment_queues where application_id = '${appId}' order by id desc limit 1`);

    // Swarm: a fixture destination on the second server, marked as a manager only for this step.
    psql(`update server_settings set is_swarm_manager = true where server_id = ${srv2}`);
    const swarmNet = psql(`insert into swarm_dockers (uuid, name, network, server_id, created_at, updated_at) values ('opssw${h.label}${stamp}', 'ops-swarm', 'ops-sw-${h.label}', ${srv2}, now(), now()) returning id`).split('\n')[0];
    const sw = await newApp(`swarm-${h.label}`);
    psql(`update applications set destination_type = 'App\\Models\\SwarmDocker', destination_id = ${swarmNet} where uuid = '${sw}'`);
    await h.go(sect(sw, 'swarm'));
    await h.page.waitForTimeout(1500);
    await h.shot('swarm');
    rec.swarm_menu = await h.page.locator('.menu-item-label:visible').allInnerTexts();
    await h.field('Replicas').fill('3');
    await h.page.locator('textarea:visible').first().fill('placement:\n  constraints: []');
    await h.page.waitForTimeout(300);
    await h.field('Replicas').press('Enter');
    rec.swarm_toast = await h.toasts(2000);
    rec.swarm_row = psql(`select a.swarm_replicas, a.swarm_placement_constraints, s.is_swarm_only_worker_nodes from applications a join application_settings s on s.application_id = a.id where a.uuid = '${sw}'`);
    psql(`update applications set destination_type = 'App\\Models\\StandaloneDocker', destination_id = ${net2} where uuid = '${sw}'`);
    psql(`delete from swarm_dockers where id = ${swarmNet}`);
    psql(`update server_settings set is_swarm_manager = false where server_id = ${srv2}`);
    return rec;
  });

  // The member role.
  const seen = apps[0];
  await flow(port, 'bob', async (h) => {
    const rec = {};
    h.page.setDefaultTimeout(10000);
    for (const s of ['resource-operations', 'domains', 'servers']) {
      await h.go(sect(seen, s));
      await h.page.waitForTimeout(1500);
      await h.shot(`member-${s}`);
      rec[s] = (await h.page.locator('.application-settings-workspace').first().innerText().catch(() => '')).replace(/\s+/g, ' ').replace(/[a-z0-9]{24}/g, 'U').replaceAll(stamp, 'S').slice(0, 800);
    }
    await h.go(sect(seen, 'files'));
    await h.page.waitForTimeout(1500);
    await h.shot('member-files');
    rec.files_text = await h.page.locator('text=403').count();
    return rec;
  });
} finally {
  for (const a of apps) await api('DELETE', `/applications/${a}`).catch(() => {});
  await new Promise((r) => setTimeout(r, 20000));
  await api('DELETE', `/projects/${P}`).catch((e) => console.log('leftover project', P, e.message));
  psql(`delete from standalone_dockers where id = ${net2}`);
  psql(`delete from server_settings where server_id = ${srv2}`);
  psql(`delete from servers where id = ${srv2}`);
  vm(`docker rmi opsimg${stamp}:abcdef1234 opsimg${stamp}:abcdef5678 >/dev/null 2>&1 || true`);
}
