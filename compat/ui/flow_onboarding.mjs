// The onboarding wizard on both sides with a fresh test user, through the SSH test host: reference at
// `th-root`, coolify-rs at 127.0.0.1:2223 (start the instance with COOLIFY_RS_SSH_BINARY=/usr/bin/ssh).
//   node compat/ui/flow_onboarding.mjs 18086 /path/to/testing-host/key
import fs from 'node:fs';
import { flow, psql, auditMax, auditSince } from './flow.mjs';
import { createUser, deleteUser, tinker } from './flow_test_users.mjs';

const [port, keyFile] = process.argv.slice(2);
const key = fs.readFileSync(keyFile, 'utf8');
const user = 'batd3';
let team;
let uid;

const clean = (s) => s.replace(/\s+/g, ' ').trim();
const stripIds = (r) => JSON.parse(JSON.stringify(r).replace(/"(uuid|id|private_key_id|server_id|fingerprint|created_at|updated_at|sentinel_token|sentinel_custom_url)":("[^"]*"|\d+|null)/g, '"$1":"x"'));

function setup() {
  uid = createUser(user);
  team = psql(`select t.id from teams t join team_user tu on tu.team_id=t.id where tu.user_id=${uid} limit 1`);
  psql(`update teams set show_boarding=true where id=${team}`);
  tinker(`Illuminate\\Support\\Facades\\Cache::forget("user:${uid}:team:${team}");`);
}
function teardown() {
  tinker(`App\\Models\\Server::where("team_id",${team})->get()->each(function ($s) { $s->settings?->delete(); $s->standaloneDockers()->delete(); $s->forceDelete(); }); App\\Models\\Project::where("team_id",${team})->get()->each->delete(); App\\Models\\PrivateKey::where("team_id",${team})->get()->each->delete();`);
  psql(`delete from audit_events where team_id=${team}`);
  deleteUser(user);
}

await flow(
  port,
  `${user}`,
  async (h) => {
    const r = {};
    const start = auditMax();
    await h.page.route('**vultr.com**', (route) => route.abort());
    await h.page.waitForTimeout(1500);
    r.landed = new URL(h.page.url()).pathname;
    await h.shot('b-welcome', true);
    await h.page.getByRole('button', { name: 'Contact support' }).first().click();
    await h.page.waitForTimeout(400);
    await h.shot('b-help', false);
    await h.page.keyboard.press('Escape');
    await h.page.getByRole('button', { name: 'Continue', exact: true }).first().click();
    await h.page.waitForTimeout(800);
    r.step1 = new URL(h.page.url()).search;
    await h.shot('b-type', true);
    // Remote.
    await h.page.getByRole('button', { name: /IP address or domain/ }).first().click();
    await h.page.waitForTimeout(800);
    r.step2 = new URL(h.page.url()).searchParams.get('step');
    await h.shot('b-key', true);
    await h.page.getByRole('button', { name: /Generate new key/ }).first().click();
    await h.page.waitForSelector('textarea:visible', { timeout: 120000 });
    await h.page.waitForTimeout(500);
    await h.page.evaluate(() => document.querySelectorAll('textarea').forEach((t) => (t.style.color = 'transparent')));
    r.generatedLabels = await h.page.locator('label:visible').allInnerTexts();
    await h.page.evaluate(() => {
      const n = document.querySelector('input[id=privateKeyName], input[placeholder*="production-server-key"]');
      if (n) n.style.color = 'transparent';
    });
    await h.shot('b-key-generate', true);
    await h.page.goBack();
    await h.page.waitForTimeout(800);
    await h.go(`/onboarding?step=private-key&selectedServerType=remote`);
    await h.page.getByRole('button', { name: /Use existing key/ }).first().click();
    await h.page.waitForTimeout(800);
    await h.shot('b-key-own', true);
    await h.page.getByRole('button', { name: /Save SSH Key/ }).click();
    await h.page.waitForTimeout(800);
    r.keyMissing = await h.page.locator('textarea:visible').first().evaluate((t) => t.validationMessage);
    await h.page.locator('textarea:visible').first().fill('not a key');
    await h.page.getByRole('button', { name: /Save SSH Key/ }).click();
    await h.page.waitForTimeout(1000);
    r.keyInvalid = clean(await h.page.locator('section').first().innerText()).includes('invalid');
    await h.page.locator('input[placeholder*="production-server-key"]').fill('batd-key');
    await h.page.locator('input[placeholder*="what this key"]').fill('batd key');
    await h.page.locator('textarea:visible').first().fill(key);
    await h.page.getByRole('button', { name: /Save SSH Key/ }).click();
    await h.page.waitForTimeout(1200);
    r.step3 = new URL(h.page.url()).searchParams.get('step');
    r.keyRow = stripIds(JSON.parse(psql(`select row_to_json(x) from (select name,description,is_git_related,team_id is not null as t from private_keys where team_id=${team}) x`)));
    await h.page.evaluate(() => {
      const n = document.querySelector('input[placeholder*="production-app-server"]');
      if (n) n.style.color = 'transparent';
    });
    await h.shot('b-server', true);
    // Validation errors.
    await h.page.locator('input[placeholder*="production-app-server"]').fill('batd-server');
    await h.page.locator('input[placeholder*="hostname"]').fill('203.0.113.50');
    await h.page.getByRole('button', { name: /Validate Connection/ }).click();
    r.dupIp = await h.toasts();
    await h.page.getByRole('button', { name: 'Advanced Settings' }).click();
    await h.page.waitForTimeout(200);
    await h.shot('b-server-adv', true);
    await h.page.locator('input[placeholder*="production-app-server"]').fill('batd-server');
    await h.page.locator('input[placeholder*="hostname"]').fill(h.label === 'ref' ? 'th-root' : '127.0.0.1');
    await h.page.locator('input[placeholder="Default: root"]').fill('bad user');
    await h.page.getByRole('button', { name: /Validate Connection/ }).click();
    await h.page.waitForTimeout(800);
    r.badUser = await h.page.locator('text=The SSH User may only contain').count();
    await h.page.locator('input[placeholder="Default: root"]').fill('root');
    if (h.label === 'rs') await h.page.locator('input[placeholder="Default: 22"]').fill('2223');
    await h.page.getByRole('button', { name: /Validate Connection/ }).click();
    await h.page.waitForTimeout(1500);
    r.step4 = new URL(h.page.url()).searchParams.get('step');
    r.serverRow = stripIds(JSON.parse(psql(`select row_to_json(x) from (select name,description,"user",proxy,team_id is not null as t from servers where team_id=${team}) x`)));
    r.settingsRow = JSON.parse(psql(`select row_to_json(x) from (select is_reachable,is_usable,is_cloudflare_tunnel,server_role,is_build_server from server_settings where server_id=(select id from servers where team_id=${team})) x`));
    r.dockers = JSON.parse(psql(`select coalesce(json_agg(x),'[]') from (select name,network from standalone_dockers where server_id=(select id from servers where team_id=${team})) x`));
    await h.shot('b-validate', true);
    await h.page.getByRole('button', { name: 'Start validation' }).click();
    await h.page.waitForTimeout(1500);
    await h.shot('b-validate-dialog', false);
    await h.page.waitForFunction(() => new URLSearchParams(location.search).get('step') === 'create-project', null, { timeout: 90000 }).catch(() => {});
    r.step5 = new URL(h.page.url()).searchParams.get('step');
    r.settingsRow2 = JSON.parse(psql(`select row_to_json(x) from (select is_reachable,is_usable from server_settings where server_id=(select id from servers where team_id=${team})) x`));
    r.proxy = JSON.parse(psql(`select proxy from servers where team_id=${team}`));
    await h.shot('b-project', true);
    await h.page.getByRole('button', { name: /Create "My First Project"/ }).click();
    await h.page.waitForTimeout(1200);
    r.step6 = new URL(h.page.url()).searchParams.get('step');
    r.projectRow = JSON.parse(psql(`select row_to_json(x) from (select p.name, e.name env from projects p join environments e on e.project_id=p.id where p.team_id=${team}) x`));
    await h.shot('b-done', true);
    r.doneText = clean(await h.page.locator('main').first().innerText());
    await h.page.getByRole('button', { name: /Deploy your first resource/ }).click();
    await h.page.waitForTimeout(1500);
    r.final = new URL(h.page.url()).pathname.replace(/[a-z0-9]{24}/g, 'U') + new URL(h.page.url()).search.replace(/\d+/, 'N');
    r.boarding = psql(`select show_boarding from teams where id=${team}`);
    const rows = auditSince(start).filter((a) => a.team_id == team || a.actor_id == uid);
    r.audit = rows.map((a) => a.event);
    if (h.label === 'ref') console.log(JSON.stringify(rows.map((a) => [a.event, a.team_id, a.resource_type, a.resource_name, a.description, a.metadata, a.has_changes])));
    return r;
  },
  { before: setup, after: teardown },
);
