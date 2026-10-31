// Keys and tokens pages: same steps on both UIs, comparing toasts, rows and audit_events.
//   node compat/ui/flow_security.mjs [port] [section ...]   sections: api cloud scripts integration member
// A test user is created for the API token steps and removed again; fixture tokens are never touched.
// Nothing goes to an external service: provider tokens are seeded in the database, and the Vault
// checks go to a local fake.
import http from 'node:http';
import { execFileSync } from 'node:child_process';
import { flow, psql, auditMax, auditSince } from './flow.mjs';

const port = process.argv[2] ?? '18086';
const only = process.argv.slice(3);
const want = (s) => !only.length || only.includes(s);
const ip = execFileSync('ipconfig', ['getifaddr', 'en0'], { encoding: 'utf8' }).trim();
const USER = 'flowtokens';
const EMAIL = `${USER}@example.test`;

const createUser = () => {
  psql(
    `insert into users (name, email, password, email_verified_at, created_at, updated_at) select 'Flow Tokens', '${EMAIL}', password, now(), now(), now() from users where email = 'alice@example.test'`,
  );
  psql(`insert into team_user (team_id, user_id, role) select 4, id, 'owner' from users where email = '${EMAIL}'`);
};
const dropUser = () => {
  psql(`delete from personal_access_tokens where tokenable_type = 'App\\Models\\User' and tokenable_id in (select id from users where email = '${EMAIL}')`);
  psql(`delete from team_user where user_id in (select id from users where email = '${EMAIL}')`);
  psql(`delete from users where email = '${EMAIL}'`);
};
const uid = () => psql(`select id from users where email = '${EMAIL}'`);
const norm = (rows) => rows.filter((r) => /flow/.test(JSON.stringify(r.metadata))).map((r) => ({ ...r, actor_id: 'U', metadata: maskMeta(r.metadata) }));
const maskMeta = (m) => {
  const out = { ...m };
  for (const k of ['cloud_init_script_id', 'integration_token_uuid', 'cloud_token_uuid']) if (k in out) out[k] = 'X';
  if (typeof out.expires_at === 'string') out.expires_at = 'T';
  return out;
};

if (want('api')) {
  console.log('== api tokens');
  const tokens = () =>
    psql(
      `select coalesce(json_agg(r order by r.name), '[]') from (select tokenable_type, name, team_id, abilities, length(token) as len, expires_at is not null as expires, extract(day from expires_at - created_at) as days, last_used_at is not null as used from personal_access_tokens where tokenable_id = ${uid()} and tokenable_type = 'App\\Models\\User') r`,
    );
  await flow(
    port,
    USER,
    async (h) => {
      const rec = {};
      const start = auditMax();
      await h.go('/security/api-tokens');
      await h.shot('sc-api-empty');
      const label = () => h.page.locator('button.listbox-trigger:visible', { hasText: 'Selected permissions' }).innerText();
      await h.field('Description').fill('ab');
      await h.button('Create token').click();
      rec.short = await h.toasts(1200);
      await h.page.locator('button.listbox-trigger:visible', { hasText: 'Selected permissions' }).click();
      await h.page.waitForTimeout(200);
      await h.shot('sc-api-perms', false);
      const check = async (name) => {
        const order = ['root', 'write', 'deploy', 'read', 'read-sensitive'];
        await h.page.locator('.listbox-panel:visible .listbox-option input[type=checkbox]').nth(order.indexOf(name)).click({ force: true });
        await h.page.waitForTimeout(700);
        return label();
      };
      rec.p_deploy = await check('deploy');
      rec.p_sensitive = await check('read-sensitive');
      rec.p_read_off = await check('read');
      rec.p_root = await check('root');
      rec.p_root_off = await check('root');
      rec.p_write = await check('write');
      await h.page.keyboard.press('Escape');
      await h.field('Description').fill('flow token one');
      await h.button('Create token').click();
      rec.create = await h.toasts(1500);
      rec.rows = tokens();
      await h.shot('sc-api-created');
      const plain = await h.page.locator('input[readonly].font-mono').inputValue();
      rec.format = /^\d+\|[A-Za-z0-9]{40}[0-9a-f]{8}$/.test(plain);
      const base = h.base;
      rec.works = (await fetch(`${base}/api/v1/teams/current`, { headers: { Authorization: `Bearer ${plain}` } })).status;
      await h.listbox('Expires in', 'Never');
      await h.field('Description').fill('flow token two');
      await h.button('Create token').click();
      rec.create2 = await h.toasts(1500);
      rec.rows2 = tokens();
      await h.go('/security/api-tokens');
      await h.shot('sc-api-list');
      await h.page.locator('button:visible', { hasText: /^\s*Revoke\s*$/ }).first().click();
      await h.page.waitForTimeout(400);
      await h.shot('sc-api-revoke', false);
      const name = await h.page.locator('input[readonly]:visible, input:disabled:visible').first().inputValue().catch(() => 'flow token two');
      await h.page.locator('input:visible[type=text]:not([readonly]), input:visible:not([type]):not([readonly])').last().fill(name);
      await h.page.locator('button:visible', { hasText: 'Revoke token' }).last().click();
      rec.revoke = await h.toasts(1500);
      rec.rows3 = tokens();
      rec.audit = norm(auditSince(start, '^ui\\.api_token'));
      return rec;
    },
    {
      before: () => {
        dropUser();
        createUser();
      },
      after: dropUser,
    },
  );
}

const seedCloudToken = () =>
  execFileSync('docker', ['exec', 'coolify-rs-reference-coolify-1', 'php', '/var/www/html/artisan', 'tinker', '--execute', `App\\Models\\CloudProviderToken::query()->create(["team_id" => 4, "provider" => "digitalocean", "token" => "not-a-real-token", "name" => "flow-cloud", "description" => "seeded"]);`], { encoding: 'utf8' });

if (want('cloud')) {
  console.log('== cloud tokens');
  const rows = () => psql(`select coalesce(json_agg(r order by r.name), '[]') from (select name, provider, description, team_id, uuid is not null as has_uuid from cloud_provider_tokens where name like 'flow-%') r`);
  const cleanup = () => psql(`delete from cloud_provider_tokens where name like 'flow-%'`);
  await flow(
    port,
    'alice',
    async (h) => {
      const rec = {};
      const start = auditMax();
      cleanup();
      await h.go('/security/cloud-tokens');
      await h.shot('sc-cloud-empty');
      await h.button('New token').click();
      await h.page.waitForTimeout(400);
      await h.shot('sc-cloud-new', false);
      await h.listbox('Provider', 'DigitalOcean');
      await h.page.waitForTimeout(300);
      rec.placeholder = await h.field('Token name').getAttribute('placeholder');
      await h.field('Token name').fill('   ');
      await h.field('API token').fill('   ');
      await h.page.locator('button:visible', { hasText: 'Validate and add' }).click();
      await h.page.waitForTimeout(900);
      rec.errors = await h.page.locator('.text-red-500:visible').allInnerTexts();
      await h.shot('sc-cloud-errors', false);
      seedCloudToken();
      const uuid = psql(`select uuid from cloud_provider_tokens where name = 'flow-cloud'`);
      await h.go('/security/cloud-tokens');
      await h.shot('sc-cloud-list');
      await h.page.locator('.icon-button:visible[aria-label="Edit flow-cloud"]').click();
      await h.page.waitForTimeout(500);
      await h.shot('sc-cloud-edit', false);
      await h.field('Description').fill('  edited  ');
      await h.page.locator('button:visible[type=submit]', { hasText: 'Save changes' }).click();
      rec.save = await h.toasts(1200);
      rec.save_rows = rows();
      await h.go(`/security/cloud-tokens/${uuid}`);
      await h.shot('sc-cloud-show');
      await h.field('Name').fill('flow-cloud2');
      await h.field('Name').press('Enter');
      await h.page.waitForTimeout(200);
      rec.save2 = await h.toasts(1200);
      rec.save2_rows = rows();
      await h.go(`/security/cloud-tokens/${uuid}`);
      await h.page.locator('button:visible', { hasText: /^\s*Delete\s*$/ }).first().click();
      await h.page.waitForTimeout(400);
      await h.shot('sc-cloud-delete', false);
      await h.page.locator('input:visible[type=text]:not([readonly]), input:visible:not([type]):not([readonly])').last().fill('flow-cloud2');
      await h.page.locator('button:visible', { hasText: 'Delete token' }).last().click();
      await h.page.waitForTimeout(1500);
      rec.deleted_path = new URL(h.page.url()).pathname;
      rec.deleted_rows = rows();
      rec.audit = norm(auditSince(start, '^ui\\.cloud_token'));
      return rec;
    },
    { after: cleanup },
  );
}

if (want('scripts')) {
  console.log('== cloud-init scripts');
  const rows = () => psql(`select coalesce(json_agg(r order by r.name), '[]') from (select name, team_id, script is not null as has_script, uuid is not null as has_uuid from cloud_init_scripts where name like 'flow-%') r`);
  const cleanup = () => psql(`delete from cloud_init_scripts where name like 'flow-%'`);
  await flow(
    port,
    'alice',
    async (h) => {
      const rec = {};
      const start = auditMax();
      await h.go('/security/cloud-init-scripts');
      await h.shot('sc-scripts-empty');
      await h.button('New script').click();
      await h.page.waitForTimeout(400);
      await h.shot('sc-scripts-new', false);
      await h.field('Script name').fill('flow-script');
      await h.page.locator('textarea:visible').fill('a: [unclosed');
      await h.page.locator('button:visible', { hasText: 'Create script' }).click();
      await h.page.waitForTimeout(900);
      rec.bad_yaml = await h.page.locator('.text-red-500:visible').allInnerTexts();
      await h.shot('sc-scripts-errors', false);
      await h.page.locator('textarea:visible').fill('#!/bin/bash\necho hi');
      await h.page.locator('button:visible', { hasText: 'Create script' }).click();
      rec.create = await h.toasts(1200);
      rec.rows = rows();
      await h.page.waitForTimeout(400);
      // The form dispatches `closeModal`, which nothing listens to: the modal stays open, empty.
      await h.shot('sc-scripts-after-create', false);
      await h.page.locator('button[aria-label="Close"]:visible').first().click({ timeout: 2000 }).catch(() => {});
      await h.page.keyboard.press('Escape');
      await h.page.waitForTimeout(400);
      await h.shot('sc-scripts-list');
      const uuid = psql(`select uuid from cloud_init_scripts where name = 'flow-script'`);
      await h.page.locator('.icon-button:visible[aria-label="Edit flow-script"]').click();
      await h.page.waitForTimeout(500);
      await h.shot('sc-scripts-edit', false);
      await h.field('Script name').fill('flow-script2');
      await h.page.locator('button:visible[type=submit]', { hasText: 'Save changes' }).click();
      rec.save = await h.toasts(1200);
      rec.save_rows = rows();
      await h.go(`/security/cloud-init-scripts/${uuid}`);
      await h.shot('sc-scripts-show');
      rec.value = await h.page.locator('textarea:visible').inputValue();
      await h.page.locator('button:visible', { hasText: /^\s*Delete\s*$/ }).first().click();
      await h.page.waitForTimeout(400);
      await h.page.locator('input:visible[type=text]:not([readonly]), input:visible:not([type]):not([readonly])').last().fill('flow-script2');
      await h.page.locator('button:visible', { hasText: 'Delete script' }).last().click();
      await h.page.waitForTimeout(1500);
      rec.deleted_path = new URL(h.page.url()).pathname;
      rec.deleted_rows = rows();
      rec.audit = norm(auditSince(start, '^ui\\.cloud_init_script'));
      return rec;
    },
    { before: cleanup, after: cleanup },
  );
}

if (want('integration')) {
  console.log('== integration tokens');
  const FAKE = 19078;
  const server = http.createServer((req, res) => {
    res.writeHead(req.headers['x-vault-token'] === 'bad' ? 403 : 200, { 'Content-Type': 'application/json' });
    res.end('{"data":{}}');
  });
  await new Promise((r) => server.listen(FAKE, '0.0.0.0', r));
  const instance = psql('select webhook_allowed_internal_hosts from instance_settings where id = 0');
  psql(`update instance_settings set webhook_allowed_internal_hosts = '["${ip}"]' where id = 0`);
  const rows = () => psql(`select coalesce(json_agg(r order by r.name), '[]') from (select name, provider, capabilities, metadata, team_id from integration_tokens where name like 'flow-%') r`);
  const cleanup = () => psql(`delete from integration_tokens where name like 'flow-%'`);
  try {
    await flow(
      port,
      'alice',
      async (h) => {
        const rec = {};
        const start = auditMax();
        await h.go('/security/integration-tokens');
        await h.shot('sc-int-empty');
        await h.button('New token').click();
        await h.page.waitForTimeout(400);
        await h.shot('sc-int-new-cloudflare');
        await h.listbox('Provider', 'Doppler');
        await h.page.waitForTimeout(500);
        await h.shot('sc-int-new-doppler');
        await h.field('Token name').fill('flow-doppler');
        await h.field('API token').fill('wrong');
        await h.page.locator('button:visible', { hasText: 'Validate and add' }).click();
        await h.page.waitForTimeout(800);
        rec.doppler_regex = await h.page.locator('.text-red-500:visible').allInnerTexts();
        await h.listbox('Provider', 'Infisical');
        await h.page.waitForTimeout(500);
        await h.shot('sc-int-new-infisical');
        await h.listbox('Provider', 'HashiCorp Vault');
        await h.page.waitForTimeout(500);
        await h.shot('sc-int-new-vault');
        await h.field('Token name').fill('flow-vault');
        await h.field('Vault token').fill('bad');
        await h.field('Base URL').fill('not a url');
        await h.page.locator('button:visible', { hasText: 'Validate and add' }).click();
        await h.page.waitForTimeout(800);
        rec.vault_bad_url = await h.page.locator('.text-red-500:visible').allInnerTexts();
        await h.field('Base URL').fill(`http://${ip}:${FAKE}`);
        await h.page.locator('button:visible', { hasText: 'Validate and add' }).click();
        rec.vault_refused = await h.toasts(1500);
        await h.field('Vault token').fill('good');
        await h.field('Namespace').fill('team-a');
        await h.page.locator('button:visible', { hasText: 'Validate and add' }).click();
        rec.vault_ok = await h.toasts(1800);
        rec.rows = rows();
        await h.page.waitForTimeout(400);
        await h.shot('sc-int-list');
        await h.page.locator('.icon-button:visible[aria-label="Edit flow-vault"]').click();
        await h.page.waitForTimeout(500);
        await h.shot('sc-int-edit', false);
        await h.field('Token name').fill(psql("select name from integration_tokens where name like 'flow-vault%'"));
        await h.page.locator('button:visible', { hasText: 'Validate and save' }).click();
        rec.rename = await h.toasts(1500);
        rec.rename_rows = rows();
        await h.field('Base URL').fill(`http://${ip}:${FAKE}/x`);
        await h.page.locator('button:visible', { hasText: 'Validate and save' }).click();
        await h.page.waitForTimeout(900);
        rec.reenter = await h.page.locator('.text-red-500:visible').allInnerTexts();
        await h.field('New API token').fill('good');
        await h.page.locator('button:visible', { hasText: 'Validate and save' }).click();
        rec.rotate = await h.toasts(1800);
        rec.rotate_rows = rows();
        await h.go('/security/integration-tokens');
        await h.page.locator('.icon-button:visible[aria-label^="Edit flow-vault"]').click();
        await h.page.waitForTimeout(500);
        await h.page.locator('button:visible', { hasText: /^\s*Delete\s*$/ }).first().click();
        await h.page.waitForTimeout(400);
        await h.shot('sc-int-delete', false);
        await h.page.locator('input:visible[type=text]:not([readonly]), input:visible:not([type]):not([readonly])').last().fill(psql("select name from integration_tokens where name like 'flow-vault%'"));
        await h.page.locator('button:visible', { hasText: 'Delete token' }).last().click();
        rec.delete = await h.toasts(1500);
        rec.delete_rows = rows();
        rec.audit = norm(auditSince(start, '^ui\\.integration_token'));
        return rec;
      },
      { before: cleanup, after: cleanup },
    );
  } finally {
    server.close();
    psql(`update instance_settings set webhook_allowed_internal_hosts = ${instance === '' ? 'NULL' : `'${instance}'`} where id = 0`);
  }
}

if (want('member')) {
  console.log('== member');
  await flow(port, 'bob', async (h) => {
    await h.go('/security/api-tokens');
    await h.switchTeam("Bob Member's Team", 'Acme');
    await h.go('/security/api-tokens');
    await h.page.locator('button.listbox-trigger:visible', { hasText: 'Selected permissions' }).click();
    await h.page.waitForTimeout(200);
    await h.shot('sc-member-api');
    return { disabled: await h.page.locator('.listbox-panel input:disabled').count() };
  });
}
