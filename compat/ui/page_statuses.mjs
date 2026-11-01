// HTTP status of the documents that upstream refuses before rendering, both sides.
//   node page_statuses.mjs --rs=18088
// Creates up to three invitations and deletes them by id. The password is read from the fixture and never printed.
import { chromium } from 'playwright-core';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';

const args = process.argv.slice(2);
const rs = (args.find((a) => a.startsWith('--rs=')) ?? '--rs=18088').split('=')[1];
const secret = fs.readFileSync(new URL('../reference/secrets/ui-login.txt', import.meta.url), 'utf8').match(/^password:\s*(.+)$/m)[1].trim();
const psql = (q) => execFileSync('docker', ['exec', 'coolify-rs-reference-postgres-1', 'psql', '-U', 'coolify', '-d', 'coolify', '-Atc', q], { encoding: 'utf8' }).trim().split('\n')[0];
const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
const sides = { ref: 'http://127.0.0.1:18000', rs: `http://127.0.0.1:${rs}` };
const made = [];
const invite = (label, email, age) => {
  const uuid = `ps${label}${Date.now()}`;
  made.push(psql(`insert into team_invitations (team_id, uuid, email, role, link, via, created_at, updated_at) values (4, '${uuid}', '${email}', 'member', 'http://127.0.0.1/x', 'link', now() - interval '${age}', now()) returning id`));
  return uuid;
};
let good = 0;
let total = 0;
try {
  const other = invite('other', 'nobody-ps@example.test', '0 days');
  const wrong = invite('wrong', 'bob@example.test', '0 days');
  const paths = ['/admin', '/invitations/doesnotexist', `/invitations/${other}`, `/invitations/${wrong}`, '/invitations/a/b', '/dashboard'];
  const answers = {};
  for (const [side, base] of Object.entries(sides)) {
    const guest = await browser.newContext();
    const signedIn = await browser.newContext();
    const page = await signedIn.newPage();
    await page.goto(`${base}/login`);
    await page.fill('input[name=email]', 'alice@example.test');
    await page.fill('input[name=password]', secret);
    await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 20000 }).catch(() => {}), page.click('button[type=submit]')]);
    for (const [who, ctx] of [['guest', guest], ['alice', signedIn]]) {
      for (const p of paths) {
        const r = await ctx.request.get(base + p, { maxRedirects: 0 });
        const loc = (r.headers().location ?? '').replace(base, '');
        (answers[`${who} ${p.replace(/ps(other|wrong)\d+/, '$1')}`] ??= {})[side] = `${r.status()} ${loc}`.trim();
      }
    }
    await guest.close();
    await signedIn.close();
  }
  for (const [k, v] of Object.entries(answers)) {
    total += 1;
    if (v.ref === v.rs) good += 1;
    console.log(`${v.ref === v.rs ? 'ok  ' : 'DIFF'} ${k}: ref ${v.ref} | rs ${v.rs}`);
  }
} finally {
  await browser.close();
  for (const id of made) psql(`delete from team_invitations where id=${id}`);
  console.log(`${good}/${total}; invitations created ${made.length}, deleted by id`);
}
