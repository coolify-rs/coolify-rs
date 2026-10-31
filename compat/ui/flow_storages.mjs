// S3 storage pages against a local MinIO: same steps on both UIs, comparing toasts, rows and audit_events.
//   node compat/ui/flow_storages.mjs [port] [lan-ip]
// Starts the container coolify-rs-flow-minio and removes it again. The instance's webhook allowlist
// gets the address for the run and is put back afterwards.
import { execFileSync } from 'node:child_process';
import { flow, psql, auditMax, auditSince } from './flow.mjs';

const port = process.argv[2] ?? '18086';
const ip = process.argv[3] ?? execFileSync('ipconfig', ['getifaddr', 'en0'], { encoding: 'utf8' }).trim();
const NAME = 'coolify-rs-flow-minio';
const MINIO_PORT = '19077';
const docker = (...a) => execFileSync('docker', a, { encoding: 'utf8' }).trim();

docker('rm', '-f', NAME);
docker('run', '-d', '--name', NAME, '-p', `0.0.0.0:${MINIO_PORT}:9000`, '-e', 'MINIO_ROOT_USER=flowkey', '-e', 'MINIO_ROOT_PASSWORD=flowsecret', 'minio/minio:latest', 'server', '/data');
await new Promise((r) => setTimeout(r, 4000));
docker('exec', NAME, 'mkdir', '-p', '/data/flowbucket');
docker('exec', NAME, 'mkdir', '-p', '/data/flowbucket2');

const instance = psql('select webhook_allowed_internal_hosts from instance_settings where id = 0');
psql(`update instance_settings set webhook_allowed_internal_hosts = '["${ip}"]' where id = 0`);

const row = (name) =>
  psql(
    `select coalesce(json_agg(r order by r.name), '[]') from (select name, description, region, endpoint, bucket, is_usable, unusable_email_sent, team_id, key is not null as has_key from s3_storages where name like '${name}%') r`,
  );
const cleanup = () => {
  psql("delete from scheduled_database_backup_s3_storage where scheduled_database_backup_id in (select id from scheduled_database_backups where uuid like 'flow-backup%')");
  psql("delete from scheduled_database_backups where uuid like 'flow-backup%'");
  psql("delete from s3_storages where name like 'flow-s3%'");
};
// A database row to link a schedule to; it never starts.
const api = await fetch(`http://127.0.0.1:${port}/api/v1/databases/postgresql`, {
  method: 'POST',
  headers: { Authorization: `Bearer ${(await import('./flow.mjs')).tokens.acme_owner_root}`, 'Content-Type': 'application/json' },
  body: JSON.stringify({ server_uuid: 'hml20fsj5eles0xfiqidq8j5', project_uuid: 'j8xm2emlllhfnyebjzgyzp7z', environment_uuid: 'uj3st9yvtvzxh7rul3c3opcz', name: 'flow-db', instant_deploy: false }),
}).then((r) => r.json());
const dropDatabase = () => {
  psql(`delete from local_persistent_volumes where resource_type = 'App\\Models\\StandalonePostgresql' and resource_id in (select id from standalone_postgresqls where name = 'flow-db')`);
  psql(`delete from standalone_postgresqls where name = 'flow-db'`);
};
void api;
const audit = (start) =>
  auditSince(start, '^ui\\.(s3_storage|database\\.backup|scheduled_database_backup)').map((r) => ({
    ...r,
    metadata: { ...r.metadata, s3_storage_uuid: 'U', uuid: 'U' },
  }));

async function fillCreate(h, name, bucket) {
  await h.field('Name').fill(name);
  await h.page.locator('#endpointParts-host:visible').fill(ip);
  await h.page.locator('#endpointParts-port:visible').fill(MINIO_PORT);
  await h.page.locator('button:has-text("https"):visible').first().click();
  await h.page.waitForTimeout(150);
  await h.page.locator('.listbox-option:visible', { hasText: /^\s*http\s*$/ }).first().click();
  await h.field('Bucket').fill(bucket);
  await h.field('Access Key').fill('flowkey');
  await h.field('Secret Key').fill('flowsecret');
}

const submitCreate = (h) => h.page.locator('button:visible', { hasText: 'Validate Connection & Continue' }).click();

try {
  console.log('== owner');
  await flow(
    port,
    'alice',
    async (h) => {
      const rec = {};
      const start = auditMax();
      await h.go('/storages');
      await h.shot('st-empty');
      await h.button('New storage').click();
      await h.page.waitForTimeout(400);
      await h.shot('st-new', false);
      // Bad credentials: the connection test refuses them.
      await fillCreate(h, 'flow-s3-a', 'flowbucket');
      await h.field('Secret Key').fill('wrong-secret');
      await submitCreate(h);
      rec.create_bad_secret = await h.toasts(3000);
      rec.create_bad_secret_row = row('flow-s3');
      // Validation: a bucket that is not allowed.
      await h.field('Secret Key').fill('flowsecret');
      await h.field('Bucket').fill('a');
      await submitCreate(h);
      rec.create_bad_bucket = await h.toasts(1500);
      await h.field('Bucket').fill('flowbucket');
      await h.field('Name').fill('x');
      await submitCreate(h);
      rec.create_bad_name = await h.toasts(1500);
      await h.field('Name').fill('flow-s3-a');
      await submitCreate(h);
      await h.page.waitForURL('**/storages/*', { timeout: 15000 });
      await h.page.waitForTimeout(1200);
      rec.created = row('flow-s3');
      rec.path = new URL(h.page.url()).pathname.replace(/[^/]+$/, 'UUID');
      await h.shot('st-general');
      const path = new URL(h.page.url()).pathname;
      await h.page.locator('button:visible', { hasText: 'Validate connection' }).click();
      rec.test_ok = await h.toasts(2500);
      rec.test_ok_row = row('flow-s3');
      // Wrong key in the form: testConnection does not save the form, only is_usable.
      await h.field('Access key').fill('nope');
      await h.page.locator('button:visible', { hasText: 'Validate connection' }).click();
      rec.test_bad = await h.toasts(3000);
      rec.test_bad_row = row('flow-s3');
      await h.page.waitForTimeout(300);
      await h.shot('st-general-unusable');
      // Save with the wrong key: the transaction rolls back and the form reverts.
      await h.field('Access key').fill('nope2');
      await h.field('Access key').press('Enter');
      rec.save_bad = await h.toasts(3000);
      rec.save_bad_row = row('flow-s3');
      await h.field('Access key').fill('flowkey');
      await h.field('Description').fill('edited');
      await h.field('Description').press('Enter');
      rec.save = await h.toasts(2500);
      rec.save_row = row('flow-s3');
      await h.go(`${path}/resources`);
      await h.shot('st-resources-empty');
      await h.go(`${path}/danger`);
      await h.shot('st-danger');
      await h.go('/storages');
      await h.shot('st-list');
      await h.go(path);

      // A second storage and a schedule on the first, for the resources page.
      await h.go('/storages');
      await h.button('New storage').click();
      await h.page.waitForTimeout(300);
      await fillCreate(h, 'flow-s3-b', 'flowbucket2');
      await submitCreate(h);
      await h.page.waitForURL('**/storages/*', { timeout: 15000 });
      await h.page.waitForTimeout(800);
      const a = psql("select id from s3_storages where name = 'flow-s3-a'");
      const database = psql(`select id from standalone_postgresqls where name = 'flow-db' limit 1`);
      psql(
        `insert into scheduled_database_backups (uuid, team_id, frequency, database_type, database_id, s3_storage_id, save_s3, enabled, created_at, updated_at) values ('flow-backup-1', 4, '0 0 * * *', 'App\\Models\\StandalonePostgresql', ${database}, ${a}, true, true, now(), now()), ('flow-backup-2', 4, '0 1 * * *', 'App\\Models\\StandalonePostgresql', 999999, ${a}, true, false, now(), now())`,
      );
      await h.go(`${path}/resources`);
      await h.shot('st-resources');
      const backups = () => psql(`select coalesce(json_agg(r order by r.uuid), '[]') from (select b.uuid, b.save_s3, s.name as primary_storage, (select string_agg(x.name, ',' order by x.name) from scheduled_database_backup_s3_storage p join s3_storages x on x.id = p.s3_storage_id where p.scheduled_database_backup_id = b.id) as pivot from scheduled_database_backups b left join s3_storages s on s.id = b.s3_storage_id where b.uuid like 'flow-backup%') r`);
      rec.backups_before = backups();
      const flowRow = () => h.page.locator('div.min-h-14:visible', { hasText: 'flow-db' });
      await flowRow().locator('button', { hasText: /^\s*Move\s*$/ }).click();
      rec.move_same = await h.toasts(1000);
      await flowRow().locator('.listbox-trigger').click();
      await h.page.waitForTimeout(200);
      await h.page.locator('.listbox-option:visible', { hasText: 'flow-s3-b' }).first().click();
      await flowRow().locator('button', { hasText: /^\s*Move\s*$/ }).click();
      rec.move = await h.toasts(1200);
      rec.backups_moved = backups();
      // Upstream's rows have no wire:key, so the page is reloaded before the next action.
      await h.go(`${path}/resources`);
      await h.page.locator('button:visible', { hasText: /^\s*Disable\s*$/ }).first().click();
      await h.page.waitForTimeout(400);
      await h.shot('st-disable', false);
      await h.page.locator('button:visible', { hasText: 'Remove storage' }).last().click();
      rec.disable = await h.toasts(1500);
      rec.backups_disabled = backups();
      await h.go(`${path}/danger`);
      await h.page.locator('button:visible', { hasText: /^\s*Delete\s*$/ }).first().click();
      await h.page.waitForTimeout(400);
      await h.shot('st-delete', false);
      await h.page.locator('input:visible[type=text], input:visible:not([type])').last().fill('flow-s3-a');
      await h.page.locator('button:visible', { hasText: 'Permanently Delete' }).last().click();
      await h.page.waitForTimeout(1500);
      rec.deleted_url = new URL(h.page.url()).pathname;
      rec.deleted_rows = row('flow-s3');
      rec.backups_deleted = backups();
      rec.audit = audit(start);
      return rec;
    },
    { before: cleanup, after: cleanup },
  );

  console.log('== member');
  const token = (await import('./flow.mjs')).tokens.acme_owner_root;
  await flow(
    port,
    'bob',
    async (h) => {
      const created = await fetch(`http://127.0.0.1:${port}/api/v1/s3-storages`, {
        method: 'POST',
        headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
        body: JSON.stringify({ name: 'flow-s3-m', endpoint: `http://${ip}:${MINIO_PORT}`, bucket: 'flowbucket', region: 'us-east-1', key: 'flowkey', secret: 'flowsecret', is_usable: true }),
      }).then((r) => r.json());
      await h.go('/storages');
      await h.switchTeam("Bob Member's Team", 'Acme');
      await h.go('/storages');
      await h.shot('st-member-list');
      await h.go(`/storages/${created.uuid}`);
      await h.shot('st-member-general');
      await h.go(`/storages/${created.uuid}/danger`);
      await h.shot('st-member-danger');
      return { ok: !!created.uuid, new_button: await h.page.locator('button:visible', { hasText: 'New storage' }).count() };
    },
    { before: cleanup, after: cleanup },
  );
} finally {
  cleanup();
  dropDatabase();
  psql(`update instance_settings set webhook_allowed_internal_hosts = ${instance === '' ? 'NULL' : `'${instance}'`} where id = 0`);
  docker('rm', '-f', NAME);
}
