// Differential test of /canvas/api: the same browser-session requests on the reference and coolify-rs.
//   COOLIFY_RS_BASE=http://127.0.0.1:18092 node compat/diff/canvas_diff.mjs
// Creates one project with an application, a database, a service, a tag and shared variables through
// the reference API, compares every route, then deletes exactly what it created (by uuid and id).
// The login password is read from the fixture file and never printed.
import { createRequire } from 'node:module';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';

const ROOT = new URL('../', import.meta.url);
const require = createRequire(new URL('ui/package.json', ROOT));
const { chromium } = require('playwright-core');

const REF = 'http://127.0.0.1:18000';
const RS = process.env.COOLIFY_RS_BASE ?? 'http://127.0.0.1:18092';
const tokens = JSON.parse(fs.readFileSync(new URL('reference/secrets/tokens.json', ROOT), 'utf8'));
const secret = fs.readFileSync(new URL('reference/secrets/ui-login.txt', ROOT), 'utf8').match(/^password:\s*(.*)$/m)[1].trim();
const TEAM = String(tokens.acme_team_id);

const sql = (q) =>
  execFileSync('docker', ['exec', 'coolify-rs-reference-postgres-1', 'psql', '-U', 'coolify', '-d', 'coolify', '-Atc', q], { encoding: 'utf8' }).trim();

async function refApi(method, path, body) {
  for (let i = 0; i < 40; i++) {
    const r = await fetch(`${REF}/api/v1${path}`, {
      method,
      headers: { Authorization: `Bearer ${tokens.acme_owner_root}`, Accept: 'application/json', ...(body ? { 'Content-Type': 'application/json' } : {}) },
      body: body ? JSON.stringify(body) : undefined,
    });
    if (r.status === 429) {
      await new Promise((d) => setTimeout(d, 5000));
      continue;
    }
    const text = await r.text();
    if (r.status >= 400) throw new Error(`${method} ${path}: ${r.status} ${text.slice(0, 200)}`);
    return text ? JSON.parse(text) : null;
  }
  throw new Error('rate limited');
}

const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });

async function login(base, user) {
  const ctx = await browser.newContext();
  const page = await ctx.newPage();
  await page.goto(`${base}/login`);
  await page.fill('input[name=email]', `${user}@example.test`);
  await page.fill('input[name=password]', secret);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 20000 }).catch(() => {}), page.keyboard.press('Enter')]);
  if (new URL(page.url()).pathname.startsWith('/login')) throw new Error(`login failed on ${base} for ${user}`);
  await page.goto(`${base}/servers/canvas`);
  await page.waitForSelector('meta[name="csrf-token"]', { state: 'attached', timeout: 20000 });
  return page;
}

async function call(page, method, path, body, { team = TEAM, raw } = {}) {
  return page.evaluate(
    async ({ method, path, body, team, raw }) => {
      const headers = {
        Accept: 'application/json',
        'X-Requested-With': 'XMLHttpRequest',
        'X-CSRF-TOKEN': document.querySelector('meta[name="csrf-token"]')?.content || '',
      };
      if (team !== null) headers['X-Canvas-Team'] = team;
      if (body !== undefined || raw !== undefined) headers['Content-Type'] = 'application/json';
      const r = await fetch(`/canvas/api${path}`, { method, headers, credentials: 'same-origin', body: raw ?? (body === undefined ? undefined : JSON.stringify(body)) });
      const text = await r.text();
      let json = null;
      try {
        json = JSON.parse(text);
      } catch {}
      return { status: r.status, json, text: json === null ? text.slice(0, 200) : undefined };
    },
    { method, path, body, team, raw },
  );
}

function normalize(value, base) {
  const s = JSON.stringify(value);
  return s === undefined
    ? s
    : s
        .split(base)
        .join('BASE')
        .replace(/(\.svg)\?v=\d+/g, '$1');
}

let same = 0;
const failures = [];
function firstDiff(a, b, path = '') {
  if (typeof a !== typeof b || a === null || b === null || typeof a !== 'object') return a === b ? null : `${path}: ${JSON.stringify(a)?.slice(0, 160)} != ${JSON.stringify(b)?.slice(0, 160)}`;
  if (Array.isArray(a) !== Array.isArray(b)) return `${path}: array vs object`;
  const ka = Object.keys(a);
  const kb = Object.keys(b);
  if (ka.join() !== kb.join()) return `${path}: keys [${ka.join()}] != [${kb.join()}]`;
  for (const k of ka) {
    const d = firstDiff(a[k], b[k], `${path}.${k}`);
    if (d) return d;
  }
  return null;
}
function record(name, r, s, mask = (x) => x) {
  const a = mask(JSON.parse(normalize(r, REF)));
  const b = mask(JSON.parse(normalize(s, RS)));
  const d = firstDiff(a, b);
  if (d) failures.push(`${name}: ${d}`);
  else same++;
}

const created = { project: null, app: null, db: null, service: null, tag: null, vars: [] };
let refPage, rsPage, refMember, rsMember;

async function both(name, method, path, body, opts, mask) {
  const r = await call(refPage, method, path, body, opts);
  const s = await call(rsPage, method, path, body, opts);
  record(name, r, s, mask);
  return [r, s];
}

const row = (table, where) => sql(`SELECT row_to_json(t) FROM (SELECT * FROM ${table} WHERE ${where}) t`);
const uuidMask = (x) => {
  if (x?.json?.id) x.json.id = 'ID';
  return x;
};

try {
  const server = sql(`SELECT uuid FROM servers WHERE team_id = ${TEAM} AND deleted_at IS NULL ORDER BY id LIMIT 1`);
  created.project = (await refApi('POST', '/projects', { name: 'canvas-diff' })).uuid;
  const project = created.project;
  const projectId = sql(`SELECT id FROM projects WHERE uuid = '${project}'`);
  const env = sql(`SELECT uuid FROM environments WHERE project_id = ${projectId} ORDER BY id LIMIT 1`);
  const envId = sql(`SELECT id FROM environments WHERE uuid = '${env}'`);
  const envName = sql(`SELECT name FROM environments WHERE uuid = '${env}'`);
  created.app = (
    await refApi('POST', '/applications/dockerimage', {
      project_uuid: project,
      environment_uuid: env,
      server_uuid: server,
      docker_registry_image_name: 'nginx',
      docker_registry_image_tag: 'alpine',
      ports_exposes: '80',
      name: 'Canvas App',
      domains: 'http://canvas-diff.example.test:8080',
      instant_deploy: false,
    })
  ).uuid;
  created.db = (await refApi('POST', '/databases/redis', { project_uuid: project, environment_uuid: env, server_uuid: server, name: 'canvas-redis', instant_deploy: false })).uuid;
  created.service = (await refApi('POST', '/services', { type: 'ntfy', project_uuid: project, environment_uuid: env, server_uuid: server, name: 'canvas-svc', instant_deploy: false })).uuid;
  const emptyEnv = (await refApi('POST', `/projects/${project}/environments`, { name: 'empty-one' })).uuid;
  await refApi('POST', `/applications/${created.app}/envs`, { key: 'USES_FOO', value: '{{project.FOO}}' });
  await refApi('POST', `/applications/${created.app}/envs`, { key: 'EMBEDS_FOO', value: 'x{{project.BAR}}y' });
  await refApi('POST', `/applications/${created.app}/envs`, { key: 'GONE', value: '{{project.DELETED_ONE}}' });

  refPage = await login(REF, 'alice');
  rsPage = await login(RS, 'alice');

  // Shared variables and a tag, through coolify-rs' page routes; the rows are the same on both sides.
  const rsPost = (path, body) =>
    rsPage.evaluate(
      async ({ path, body }) => {
        const session = await (await fetch('/ui/session')).json();
        const r = await fetch(path, { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': session.csrf_token ?? session.csrf, Accept: 'application/json' }, body: JSON.stringify(body) });
        return { status: r.status, text: await r.text() };
      },
      { path, body },
    );
  for (const key of ['FOO', 'BAR', 'LINK_ME']) {
    const r = await rsPost(`/ui/shared-variables/scope/add?kind=project&uuid=${project}`, { key, value: 'v', comment: null, is_multiline: false });
    if (r.status >= 400) throw new Error(`shared variable: ${r.status} ${r.text.slice(0, 200)}`);
  }
  const tagged = await rsPost('/ui/tags', { name: `canvas-diff-${Date.now() % 100000}` });
  if (tagged.status >= 400) throw new Error(`tag: ${tagged.status} ${tagged.text.slice(0, 200)}`);
  created.tag = JSON.parse(tagged.text).name;
  const tagRow = sql(`SELECT uuid FROM tags WHERE team_id = ${TEAM} AND name = '${created.tag}'`);
  const linkMeId = sql(`SELECT id FROM shared_environment_variables WHERE project_id = ${projectId} AND key = 'LINK_ME'`);

  // Reads.
  for (const [name, path] of [
    ['projects canvas', '/projects/canvas'],
    ['project canvas', `/projects/${project}/canvas`],
    ['environment canvas by uuid', `/projects/${project}/environments/${env}/canvas`],
    ['environment canvas by name', `/projects/${project}/environments/${envName}/canvas`],
    ['empty environment canvas', `/projects/${project}/environments/${emptyEnv}/canvas`],
    ['servers canvas', '/servers/canvas'],
    ['dashboard canvas', '/dashboard/canvas'],
    ['server canvas', `/servers/${server}/canvas`],
    ['tags canvas', '/tags/canvas'],
    ['team variables', '/shared-variables/team/canvas'],
    ['project variables', `/shared-variables/project/${project}/canvas`],
    ['environment variables', `/shared-variables/environment/${env}/canvas`],
    ['server variables', `/shared-variables/server/${server}/canvas`],
    ['missing project', '/projects/nope/canvas'],
    ['missing project env', `/projects/nope/environments/${env}/canvas`],
    ['missing environment', `/projects/${project}/environments/nope/canvas`],
    ['missing server', '/servers/nope/canvas'],
    ['missing server usage', '/servers/nope/canvas/usage'],
    ['missing server containers', '/servers/nope/canvas/containers'],
    ['missing project scope', '/shared-variables/project/nope/canvas'],
    ['missing environment scope', '/shared-variables/environment/nope/canvas'],
    ['missing server scope', '/shared-variables/server/nope/canvas'],
    ['usage', `/servers/${server}/canvas/usage`],
    ['bridge projects', '/projects'],
    ['bridge environments', `/projects/${project}/environments`],
    ['bridge servers', '/servers'],
    ['bridge destinations', `/servers/${server}/destinations`],
    ['bridge keys', '/security/keys'],
    ['bridge github apps', '/github-apps'],
    ['bridge application', `/applications/${created.app}`],
    ['bridge deployment missing', '/deployments/nope'],
  ]) {
    await both(name, 'GET', path);
  }
  // The containers of a live server change while other agents run, so compare the node shape.
  {
    const [r, s] = [await call(refPage, 'GET', `/servers/${server}/canvas/containers`), await call(rsPage, 'GET', `/servers/${server}/canvas/containers`)];
    const shape = (x) => ({ status: x.status, keys: x.json?.nodes?.[0] ? Object.keys(x.json.nodes[0]) : null });
    record('containers shape', shape(r), shape(s));
  }

  // The session bridge.
  await both('no canvas team', 'GET', '/projects/canvas', undefined, { team: null });
  await both('bad canvas team', 'GET', '/projects/canvas', undefined, { team: 'abc' });
  await both('other team', 'GET', '/projects/canvas', undefined, { team: String(tokens.carol_team_id) });

  // Validation.
  const positions = `/projects/${project}/environments/${env}/canvas/positions`;
  for (const [name, method, path, opts] of [
    ['positions not json', 'PATCH', positions, { raw: 'x' }],
    ['positions empty', 'PATCH', positions, { raw: '{}' }],
    ['positions missing', 'PATCH', positions, { raw: '{"a":1}' }],
    ['positions scalar', 'PATCH', positions, { raw: '{"positions":1}' }],
    ['positions extra', 'PATCH', positions, { raw: '{"positions":{},"z":1}' }],
    ['positions bad value', 'PATCH', positions, { raw: JSON.stringify({ positions: { [created.app]: 3 } }) }],
    ['positions empty value', 'PATCH', positions, { raw: JSON.stringify({ positions: { [created.app]: [] } }) }],
    ['positions missing y', 'PATCH', positions, { raw: JSON.stringify({ positions: { [created.app]: { x: 1 } } }) }],
    ['positions not int', 'PATCH', positions, { raw: JSON.stringify({ positions: { [created.app]: { x: 'a', y: 1.5 } } }) }],
    ['positions out of range', 'PATCH', positions, { raw: JSON.stringify({ positions: { [created.app]: { x: 100001, y: -100001 } } }) }],
    ['zone positions empty', 'PATCH', '/projects/canvas/positions', { raw: '{"positions":{}}' }],
    ['zone positions bad', 'PATCH', '/projects/canvas/positions', { raw: '{"positions":{"a":{"x":"b"}}}' }],
    ['server zone positions bad', 'PATCH', '/servers/canvas/positions', { raw: '{"positions":{"a":3}}' }],
    ['dashboard positions bad', 'PATCH', '/dashboard/canvas/positions', { raw: '{"positions":1}' }],
    ['link empty', 'POST', `/projects/${project}/environments/${env}/canvas/links`, { raw: '{"a":1}' }],
    ['link same', 'POST', `/projects/${project}/environments/${env}/canvas/links`, { raw: JSON.stringify({ source: created.app, target: created.app }) }],
    ['link unknown', 'POST', `/projects/${project}/environments/${env}/canvas/links`, { raw: JSON.stringify({ source: 'a', target: 'b' }) }],
    ['link not string', 'POST', `/projects/${project}/environments/${env}/canvas/links`, { raw: JSON.stringify({ source: 1, target: [1], label: 2 }) }],
    ['link long label', 'POST', `/projects/${project}/environments/${env}/canvas/links`, { raw: JSON.stringify({ source: 'a', target: 'b', label: 'x'.repeat(65) }) }],
    ['link missing patch', 'PATCH', `/projects/${project}/environments/${env}/canvas/links/nope`, { raw: '{"label":"a"}' }],
    ['link label absent', 'PATCH', `/projects/${project}/environments/${env}/canvas/links/nope`, { raw: '{"z":"a"}' }],
    ['tag link empty', 'POST', '/tags/canvas/links', { raw: '{"a":1}' }],
    ['tag link unknown', 'POST', '/tags/canvas/links', { raw: '{"source":"a","target":"b"}' }],
    ['variable link empty', 'POST', '/shared-variables/team/canvas/links', { raw: '{"a":1}' }],
    ['variable link unknown', 'POST', `/shared-variables/project/${project}/canvas/links`, { raw: '{"source":"a","target":"b"}' }],
  ]) {
    await both(name, method, path, undefined, opts);
  }
  for (const [name, path] of [
    ['delete missing link', `/projects/${project}/environments/${env}/canvas/links/nope`],
    ['delete missing tag link', '/tags/canvas/links/nope'],
    ['delete missing variable link', `/shared-variables/project/${project}/canvas/links/nope`],
  ]) {
    await both(name, 'DELETE', path);
  }

  // Positions: each side saves, then the rows must be equal.
  const pos = (uuid, x, y) => ({ positions: { [uuid]: { x, y }, unknown: { x: 'bad' } } });
  const saved = [];
  for (const page of [refPage, rsPage]) {
    sql(`UPDATE environments SET canvas_positions = NULL WHERE id = ${envId}`);
    const r = await call(page, 'PATCH', positions, pos(created.app, 11, -22));
    const r2 = await call(page, 'PATCH', positions, { positions: { [created.db]: { x: '33', y: 44 } } });
    saved.push({ r, r2, row: JSON.parse(sql(`SELECT canvas_positions FROM environments WHERE id = ${envId}`)) });
  }
  record('environment positions', saved[0], saved[1]);
  const zones = [];
  for (const page of [refPage, rsPage]) {
    sql(`UPDATE projects SET canvas_position = NULL WHERE id = ${projectId}`);
    const r = await call(page, 'PATCH', '/projects/canvas/positions', { positions: { [project]: { x: 5, y: 6 }, unknown: { x: 1, y: 2 } } });
    zones.push({ r, row: sql(`SELECT canvas_position FROM projects WHERE id = ${projectId}`) });
  }
  record('project zone position', zones[0], zones[1]);
  await both('project canvas after moves', 'GET', '/projects/canvas');
  await both('environment canvas after moves', 'GET', `/projects/${project}/environments/${env}/canvas`);

  // Links: each side creates, updates and deletes; rows compared without ids.
  const links = `/projects/${project}/environments/${env}/canvas/links`;
  const linkRow = () => row('resource_links', `environment_id = ${envId}`).replace(/"(id|uuid|created_at|updated_at)":("[^"]*"|\d+),?/g, '');
  const steps = [];
  for (const page of [refPage, rsPage]) {
    const made = await call(page, 'POST', links, { source: created.app, target: created.db, label: ' uses ' });
    const dup = await call(page, 'POST', links, { source: created.app, target: created.db });
    const rowsAfter = linkRow();
    const view = await call(page, 'GET', `/projects/${project}/environments/${env}/canvas`);
    const relabel = await call(page, 'PATCH', `${links}/${made.json.id}`, { label: '  ' });
    const relabelled = linkRow();
    const gone = await call(page, 'DELETE', `${links}/${made.json.id}`);
    const again = await call(page, 'DELETE', `${links}/${made.json.id}`);
    const idMask = (v) => JSON.parse(JSON.stringify(v).split(made.json.id).join('LINK'));
    steps.push(idMask({ made, dup, rowsAfter, view, relabel, relabelled, gone, again, after: linkRow() }));
  }
  record('link lifecycle', { json: JSON.parse(normalize(steps[0], REF)) }, { json: JSON.parse(normalize(steps[1], RS)) });

  // Tag links.
  const tagNode = `tag:${tagRow}`;
  const tagId = sql(`SELECT id FROM tags WHERE uuid = '${tagRow}'`);
  const taggable = () => sql(`SELECT taggable_type, count(*) FROM taggables WHERE tag_id = ${tagId} GROUP BY 1`);
  const tagSteps = [];
  for (const page of [refPage, rsPage]) {
    const made = await call(page, 'POST', '/tags/canvas/links', { source: tagNode, target: created.app });
    const dup = await call(page, 'POST', '/tags/canvas/links', { source: tagNode, target: created.app });
    const rows = taggable();
    const view = await call(page, 'GET', '/tags/canvas');
    const gone = await call(page, 'DELETE', `/tags/canvas/links/${encodeURIComponent(`${tagNode}|${created.app}`)}`);
    tagSteps.push({ made, dup, rows, view, gone, after: taggable() });
  }
  record('tag link lifecycle', { json: JSON.parse(normalize(tagSteps[0], REF)) }, { json: JSON.parse(normalize(tagSteps[1], RS)) });

  // Shared variable links on an application and a service.
  const scoped = `/shared-variables/project/${project}/canvas/links`;
  const envRows = (morph, id) =>
    sql(
      `SELECT json_agg(t ORDER BY t.key, t.is_preview) FROM (SELECT key, is_preview, is_multiline, is_literal, is_runtime, is_buildtime, is_required, is_shown_once, is_shared, version, comment, "order" FROM environment_variables WHERE resourceable_type = '${morph}' AND resourceable_id = ${id}) t`,
    );
  const appId = sql(`SELECT id FROM applications WHERE uuid = '${created.app}'`);
  const svcId = sql(`SELECT id FROM services WHERE uuid = '${created.service}'`);
  const variableSteps = [];
  for (const page of [refPage, rsPage]) {
    const out = {};
    const source = `variable:${linkMeId}`;
    for (const [label, target, morph, id] of [
      ['app', created.app, 'App\\Models\\Application', appId],
      ['service', created.service, 'App\\Models\\Service', svcId],
    ]) {
      out[`${label} made`] = await call(page, 'POST', scoped, { source, target });
      out[`${label} dup`] = await call(page, 'POST', scoped, { source, target });
      out[`${label} rows`] = envRows(morph, id);
      out[`${label} view`] = await call(page, 'GET', `/shared-variables/project/${project}/canvas`);
      out[`${label} gone`] = await call(page, 'DELETE', `${scoped}/${encodeURIComponent(`${source}|${target}`)}`);
      out[`${label} after`] = envRows(morph, id);
    }
    out.embedded = await call(page, 'DELETE', `${scoped}/${encodeURIComponent(`variable:${sql(`SELECT id FROM shared_environment_variables WHERE project_id = ${projectId} AND key = 'BAR'`)}|${created.app}`)}`);
    variableSteps.push(out);
  }
  record('variable link lifecycle', { json: JSON.parse(normalize(variableSteps[0], REF)) }, { json: JSON.parse(normalize(variableSteps[1], RS)) });

  // Project icons: the reference stores through ProjectIconStorageService (its upload is Livewire's
  // temporary file flow), coolify-rs through its route. Each side serves its own file.
  const iconRow = () => JSON.parse(sql(`SELECT json_build_object('path', icon_path, 'type', icon_storage_type, 's3', icon_s3_storage_id) FROM projects WHERE id = ${projectId}`));
  const fetchIcon = (page) =>
    page.evaluate(async (path) => {
      const r = await fetch(path);
      return { status: r.status, type: r.headers.get('content-type'), cache: r.headers.get('cache-control') };
    }, `/project/${project}/icon`);
  const tinker = (code) => execFileSync('docker', ['exec', 'coolify-rs-reference-coolify-1', 'php', '/var/www/html/artisan', 'tinker', '--execute', code], { encoding: 'utf8' });
  const icons = [];
  {
    icons.push({ none: await fetchIcon(refPage) });
    const jpeg = '/9j/4AAQSkZJRgABAQEASABIAAD/2wBDAP//////////////////////////////////////////////////////////////////////////////////////wgALCAABAAEBAREA/8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQABPxA=';
    tinker(`app(\\App\\Services\\ProjectIconStorageService::class)->storeProjectContents(\\App\\Models\\Project::find(${projectId}), base64_decode('${jpeg}'));`);
    const stored = { row: iconRow(), icon: await fetchIcon(refPage) };
    tinker(`app(\\App\\Services\\ProjectIconStorageService::class)->deleteProject(\\App\\Models\\Project::find(${projectId}));`);
    icons[0] = { ...icons[0], stored, removed: iconRow() };
  }
  {
    const none = await fetchIcon(rsPage);
    const made = await rsPage.evaluate(async (project) => {
      const canvas = document.createElement('canvas');
      canvas.width = canvas.height = 64;
      canvas.getContext('2d').fillRect(0, 0, 64, 64);
      const blob = await new Promise((resolve) => canvas.toBlob(resolve, 'image/jpeg', 0.8));
      const session = await (await fetch('/ui/session')).json();
      const r = await fetch(`/ui/projects/${project}/icon`, { method: 'POST', headers: { 'Content-Type': 'image/jpeg', 'X-CSRF-Token': session.csrf_token }, body: blob });
      return { status: r.status, body: await r.json() };
    }, project);
    if (made.status !== 200 || made.body.message !== 'Project icon updated.') failures.push(`project icon upload: ${JSON.stringify(made)}`);
    const stored = { row: iconRow(), icon: await fetchIcon(rsPage) };
    const removed = await rsPage.evaluate(async (project) => {
      const session = await (await fetch('/ui/session')).json();
      return (await fetch(`/ui/projects/${project}/icon`, { method: 'DELETE', headers: { 'X-CSRF-Token': session.csrf_token } })).status;
    }, project);
    if (removed !== 200) failures.push(`project icon remove: ${removed}`);
    icons.push({ none, stored, removed: iconRow() });
  }
  // A missing icon is a 404 on both; upstream's body is the HTML error page, which an <img> never shows.
  // Its headers are Laravel's defaults for an error response, so only the status is compared.
  icons[0].none = icons[0].none.status;
  icons[1].none = icons[1].none.status;
  record('project icon lifecycle', { json: icons[0] }, { json: icons[1] });

  // A member: read only, no IPs, no editing.
  refMember = await login(REF, 'bob');
  rsMember = await login(RS, 'bob');
  const member = async (name, method, path, body) => {
    const r = await call(refMember, method, path, body);
    const s = await call(rsMember, method, path, body);
    record(`member ${name}`, r, s);
  };
  await member('servers canvas', 'GET', '/servers/canvas');
  await member('project canvas', 'GET', `/projects/${project}/canvas`);
  await member('environment canvas', 'GET', `/projects/${project}/environments/${env}/canvas`);
  await member('tags canvas', 'GET', '/tags/canvas');
  await member('team variables', 'GET', '/shared-variables/team/canvas');
  await member('positions denied', 'PATCH', positions, { positions: {} });
  await member('link denied', 'POST', links, { source: 'a', target: 'b' });
  await member('tag link denied', 'POST', '/tags/canvas/links', { source: 'a', target: 'b' });
  await member('variable link denied', 'POST', scoped, { source: 'a', target: 'b' });
  await member('bridge create denied', 'POST', '/projects', { name: 'x' });
} finally {
  // Exactly what this run created.
  const quiet = async (f) => {
    try {
      await f();
    } catch (e) {
      console.log(`cleanup: ${String(e.message).slice(0, 160)}`);
    }
  };
  const flags = '?delete_configurations=false&delete_volumes=false&docker_cleanup=false&delete_connected_networks=false';
  if (created.app) await quiet(() => refApi('DELETE', `/applications/${created.app}${flags}`));
  if (created.db) await quiet(() => refApi('DELETE', `/databases/${created.db}${flags}`));
  if (created.service) await quiet(() => refApi('DELETE', `/services/${created.service}${flags}`));
  if (created.tag) await quiet(async () => sql(`DELETE FROM tags WHERE team_id = ${TEAM} AND name = '${created.tag}'`));
  if (created.project) {
    await quiet(async () => {
      const id = sql(`SELECT id FROM projects WHERE uuid = '${created.project}'`);
      sql(`DELETE FROM shared_environment_variables WHERE project_id = ${id}`);
    });
    await new Promise((d) => setTimeout(d, 4000));
    await quiet(() => refApi('DELETE', `/projects/${created.project}`));
  }
  await browser.close();
}

console.log(`same ${same}, different ${failures.length}`);
for (const f of failures) console.log(`DIFF ${f.slice(0, 500)}`);
process.exit(failures.length ? 1 : 0);
