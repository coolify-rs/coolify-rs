// Helpers of the flow_auth*.mjs scripts: users created for one run (c-batch-*@example.test), both
// sides, screenshots and row diffs. Everything these create is removed by exact test names.
import { chromium } from 'playwright-core';
import { execFileSync } from 'node:child_process';
import crypto from 'node:crypto';
import fs from 'node:fs';

export const RS_PORT = Number(process.env.RS_PORT ?? 18086);
export const PASSWORD = 'C-batch-Pass1!';
export const out = new URL('./shots/', import.meta.url).pathname;
fs.mkdirSync(out, { recursive: true });

export const sides = [
  { name: 'ref', port: 18000 },
  { name: 'rs', port: RS_PORT },
];
export const base = (side, host = '127.0.0.1') => `http://${host}:${side.port}`;

export function psql(q) {
  return execFileSync('docker', ['exec', 'coolify-rs-reference-postgres-1', 'psql', '-U', 'coolify', '-d', 'coolify', '-tAc', q], { encoding: 'utf8' }).trim();
}

export function tinker(php) {
  return execFileSync('docker', ['exec', 'coolify-rs-reference-coolify-1', 'php', '/var/www/html/artisan', 'tinker', '--execute', php], { encoding: 'utf8' }).trim();
}

export const auditMax = () => Number(psql('select coalesce(max(id),0) from audit_events'));
export function audit(since, filter) {
  const cols = 'team_id,event,source,action,level,actor_type,actor_id,actor_name,actor_email,resource_type,resource_name,description,metadata';
  return JSON.parse(psql(`select coalesce(json_agg(r), '[]') from (select ${cols} from audit_events where id > ${since} and (${filter}) order by id) r`));
}

/** A user with a known password and a finished boarding. */
export function createUser(email, name, extra = '') {
  const hash = tinker(`echo bcrypt("${PASSWORD}");`).split('\n').pop();
  const id = tinker(
    `$u = new App\\Models\\User; $u->forceFill(["name" => "${name}", "email" => "${email}", "password" => '${hash}', "email_verified_at" => now()]); $u->save(); $t = $u->teams()->first(); $t->show_boarding = false; $t->save(); ${extra} echo $u->id;`,
  )
    .split('\n')
    .pop();
  return Number(id);
}

/** Removes every row of the test names. Other rows are not touched. */
export function cleanup() {
  const users = `select id from users where email like 'c-batch-%@example.test'`;
  // Rows without an actor (failed callbacks) are found by the browser's user agent of these flows.
  psql(`delete from audit_events where actor_email like 'c-batch-%@example.test' or metadata::text like '%c-batch-%' or resource_name like 'C Batch%' or user_agent like '%c-batch-flow%'`);
  psql(`delete from password_reset_tokens where email like 'c-batch-%@example.test'`);
  psql(`delete from oauth_identities where user_id in (${users})`);
  psql(`delete from passkeys where user_id in (${users})`);
  psql(`delete from sessions where user_id in (${users})`);
  const teams = `select team_id from team_user where user_id in (${users}) and team_id <> 0`;
  const owned = psql(`select coalesce(string_agg(team_id::text, ','), '') from team_user where team_id in (${teams})`);
  psql(`delete from team_user where user_id in (${users})`);
  if (owned) {
    for (const table of ['email_notification_settings', 'discord_notification_settings', 'slack_notification_settings', 'telegram_notification_settings', 'pushover_notification_settings', 'webhook_notification_settings']) {
      psql(`delete from ${table} where team_id in (${owned}) and team_id not in (select team_id from team_user)`);
    }
    psql(`delete from teams where id in (${owned}) and id not in (select team_id from team_user)`);
  }
  psql(`delete from users where email like 'c-batch-%@example.test'`);
}

export async function launch() {
  return chromium.launch({
    executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    headless: true,
    args: ['--host-resolver-rules=MAP host.docker.internal 127.0.0.1'],
  });
}

/** A guest context that cannot reach anything but this machine. */
export async function open(browser, { light = false } = {}) {
  const ctx = await browser.newContext({
    viewport: { width: 1440, height: 900 },
    colorScheme: light ? 'light' : 'dark',
    userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 c-batch-flow',
  });
  await ctx.addInitScript((l) => { try { localStorage.setItem('theme', l ? 'light' : 'dark'); } catch {} }, light);
  await ctx.route('**/*', (r) => {
    const host = new URL(r.request().url()).hostname;
    return ['127.0.0.1', 'localhost', 'host.docker.internal'].includes(host) ? r.continue() : r.abort();
  });
  const page = await ctx.newPage();
  return { ctx, page };
}

export async function shot(page, name, side) {
  await page.waitForTimeout(700);
  await page.screenshot({ path: `${out}${name}-${side.name}.png` });
}

export async function login(page, side, email, password = PASSWORD, host = '127.0.0.1') {
  await page.goto(base(side, host) + '/login');
  await page.waitForSelector('input[name=email]');
  await page.fill('input[name=email]', email);
  await page.fill('input[name=password]', password);
  await page.click('button[type=submit]');
  await page.waitForTimeout(1500);
}

export function totp(secret) {
  const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567';
  let bits = '';
  for (const c of secret) bits += alphabet.indexOf(c).toString(2).padStart(5, '0');
  const key = Buffer.from(bits.match(/.{8}/g).map((b) => parseInt(b, 2)));
  const step = Buffer.alloc(8);
  step.writeBigUInt64BE(BigInt(Math.floor(Date.now() / 30000)));
  const h = crypto.createHmac('sha1', key).update(step).digest();
  const o = h[h.length - 1] & 15;
  return String((h.readUInt32BE(o) & 0x7fffffff) % 1000000).padStart(6, '0');
}

/** Compares two lists of rows after `normalize`; prints only what differs. */
export function diff(label, a, b, normalize = (x) => x) {
  const x = JSON.stringify(a.map(normalize), null, 0);
  const y = JSON.stringify(b.map(normalize), null, 0);
  if (x === y) console.log(`SAME  ${label} (${a.length} rows)`);
  else console.log(`DIFF  ${label}\n  ref ${x}\n  rs  ${y}`);
}

/** The audit row without what differs by design: the per-side test name. */
export const sameShape = (r) => JSON.parse(JSON.stringify(r).replace(/c-batch-[a-z0-9]+-(ref|rs)/g, 'c-batch-X').replace(/C Batch (Ref|Rs)/g, 'C Batch X').replace(/"(team_id|actor_id)":\d+/g, '"$1":"N"'));
