// A new invitee on both sides: the magic link leads to /force-password-reset after accepting.
//   node compat/ui/flow_magic.mjs 18086 <inviting test user>
import { chromium } from 'playwright-core';
import { psql, diff } from './flow.mjs';
import { deleteUser, tinker } from './flow_test_users.mjs';

const [port, owner] = process.argv.slice(2);
const team = psql(`select t.id from teams t join team_user tu on tu.team_id=t.id join users u on u.id=tu.user_id where u.email='${owner}@example.test' and t.id<>0 order by t.id limit 1`);
const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
const results = {};
for (const [side, base] of [['ref', 'http://127.0.0.1:18000'], ['rs', `http://127.0.0.1:${port}`]]) {
  const name = 'batd-new';
  deleteUser(name);
  const token = tinker(`
    $pw = "a-random-link-password";
    $u = App\\Models\\User::create(["name"=>"${name}","email"=>"${name}@example.test","password"=>Hash::make($pw),"force_password_reset"=>true]);
    $uuid = (string) Illuminate\\Support\\Str::uuid();
    $token = Crypt::encryptString($u->email."@@@".$uuid."@@@".$pw);
    App\\Models\\TeamInvitation::create(["team_id"=>${team},"uuid"=>$uuid,"email"=>$u->email,"role"=>"member","link"=>url("/auth/link")."?token=".urlencode($token),"via"=>"link"]);
    echo $token;`).split('\n').pop();
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: 'dark' });
  const page = await ctx.newPage();
  await page.goto(`${base}/auth/link?token=${encodeURIComponent(token)}`);
  await page.waitForTimeout(1500);
  const r = { view: new URL(page.url()).pathname };
  await page.getByRole('button', { name: /accept/i }).first().click();
  await page.waitForTimeout(2500);
  r.landed = new URL(page.url()).pathname;
  r.heading = await page.locator('h1, h2').first().innerText().catch(() => '');
  await page.screenshot({ path: new URL(`./shots/l-magic-${side}.png`, import.meta.url).pathname });
  r.member = psql(`select role from team_user tu join users u on u.id=tu.user_id where u.email='${name}@example.test' and tu.team_id=${team}`);
  r.flag = psql(`select force_password_reset from users where email='${name}@example.test'`);
  results[side] = r;
  await ctx.close();
  psql(`delete from team_invitations where email='${name}@example.test'`);
  deleteUser(name);
}
await browser.close();
for (const k of Object.keys(results.ref)) diff(k, results.ref[k], results.rs[k]);
