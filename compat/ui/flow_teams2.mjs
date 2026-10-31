// Team avatar, select-team, instance users, and invitation by magic link and by email, on both UIs.
// Emails go to a local mailpit (`docker run --name batb-mailpit -p 127.0.0.1:18188:8025 -p
// 127.0.0.1:18187:1025 axllent/mailpit`); instance settings are saved first and restored exactly.
//   node compat/ui/flow_teams2.mjs [port]
import { execFileSync } from 'node:child_process';
import { flow, psql, auditMax, auditSince, saveRow, restoreRow } from './flow.mjs';
import { createUser, deleteUser, tinker } from './flow_test_users.mjs';

const port = process.argv[2] ?? '18086';
const OWNER = 'batb1';
const OTHER = 'batb2';
const VICTIM = 'batb4';
const NEWUSER = 'batb5';
const ADMIN = 'batb6';
const TEAM = 'Batb Flow Second';
const png = new URL('./shots/batb-avatar.png', import.meta.url).pathname;
execFileSync('python3', ['-I', '-c', `from PIL import Image; Image.new('RGB',(400,300),(200,30,90)).save('${png}')`]);

const rows = (table, where, cols) => psql(`select coalesce(json_agg(r), '[]') from (select ${cols} from ${table} where ${where} order by 1) r`);
const id = (n) => Number(psql(`select id from users where email='${n}@example.test'`));
const flowTeams = () => psql(`select coalesce(string_agg(id::text, ','), '') from teams where name like 'Batb Flow%'`);
const scrub = (r) => JSON.stringify(r).replace(/"invitation_uuid":"[a-z0-9]+"/g, '"invitation_uuid":"U"').replace(/"team_id":\d+/g, '"team_id":"T"').replace(/"(actor_id|member_id|deleted_user_id)":\d+/g, '"$1":"I"');
const mine = (start, names) => auditSince(start).filter((r) => names.includes(r.actor_email) && !r.event.startsWith('auth.')).map(scrub);

function cleanup() {
  const ids = flowTeams();
  if (ids) {
    psql(`delete from team_invitations where team_id in (${ids})`);
    psql(`delete from team_user where team_id in (${ids})`);
    tinker(`App\\Models\\Team::whereIn("id",[${ids}])->get()->each->delete();`);
    psql(`delete from audit_events where team_id in (${ids})`);
  }
  for (const n of [OWNER, OTHER, VICTIM, NEWUSER, ADMIN]) deleteUser(n);
}

let smtp;
function restoreSmtp() {
  if (smtp) restoreRow(smtp);
}

const shotOf = (name, label) => new URL(`./shots/${name}-${label}.png`, import.meta.url).pathname;

await flow(
  port,
  OWNER,
  async (h) => {
    const rec = {};
    const owner = id(OWNER);
    const start = auditMax();
    const team = owner && Number(psql(`select team_id from team_user where user_id=${owner} limit 1`));
    // Team avatar.
    await h.go('/team');
    await h.page.locator('input[type=file]').setInputFiles(png);
    await h.page.waitForTimeout(2000);
    rec.avatar_toasts = await h.toasts();
    await h.shot('tb2-avatar');
    rec.avatar_row = psql(`select avatar_path = 'team-avatars/' || id || '/avatar.jpg', avatar_storage_type, avatar_s3_storage_id from teams where id=${team}`);
    const src = await h.page.locator('img').evaluateAll((els) => els.map((e) => e.getAttribute('src') || '').filter((s) => s.includes('/avatar')).map((s) => s.replace(/https?:\/\/[^/]+/, '').replace(/v=\d+/, 'v=V').replace(/\/\d+\//, '/T/')));
    rec.avatar_srcs = [...new Set(src)];
    const fetched = await h.page.evaluate(async (t) => {
      const r = await fetch(`/team/${t}/avatar?v=1`);
      return [r.status, r.headers.get('content-type'), r.headers.get('cache-control')];
    }, team);
    rec.avatar_fetch = fetched;
    await h.button('Remove').click();
    rec.avatar_removed = await h.toasts();
    rec.avatar_row_after = psql(`select avatar_path, avatar_storage_type from teams where id=${team}`);
    await h.page.locator('input[type=file]').waitFor({ state: 'attached' });

    // Team Index submit.
    await h.page.locator('label:visible', { hasText: 'Description' }).first();
    const before = auditMax();
    await h.field('Description').fill('changed by flow');
    await h.field('Description').press('Enter');
    rec.team_saved = await h.toasts();
    rec.team_updated_audit = auditSince(before).filter((r) => r.actor_email === `${OWNER}@example.test`).map(scrub);

    // Instance users: a test user who is an admin of the root team.
    const browser = h.page.context().browser();
    const c = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: 'dark' });
    const a = await c.newPage();
    await a.addInitScript(() => localStorage.setItem('theme', 'dark'));
    await a.goto(h.base + '/login');
    await a.fill('input[name=email]', `${ADMIN}@example.test`);
    await a.fill('input[name=password]', h.secret);
    await Promise.all([a.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 }).catch(() => {}), a.click('button[type=submit]')]);
    const acceptPopup = () => a.locator('button:has-text("Accept And Close")').first().click({ timeout: 800 }).catch(() => {});
    await a.goto(h.base + '/team/admin');
    await a.waitForTimeout(1500);
    await acceptPopup();
    await a.screenshot({ path: shotOf('tb2-admin', h.label) });
    const search = a.locator('input[aria-label="Search users"]');
    await search.fill(VICTIM);
    await a.waitForTimeout(1200);
    await a.screenshot({ path: shotOf('tb2-admin-search', h.label) });
    rec.admin_search = (await a.locator('.data-table-row').allInnerTexts()).map((t) => t.replace(/\s+/g, ' '));
    await search.fill('nobody-has-this-name');
    await a.waitForTimeout(1200);
    await a.screenshot({ path: shotOf('tb2-admin-empty', h.label) });
    await search.fill(VICTIM);
    await a.waitForTimeout(1000);
    await a.locator('.data-table-row', { hasText: VICTIM }).locator('button:has-text("Delete")').click();
    await a.waitForTimeout(400);
    await a.screenshot({ path: shotOf('tb2-admin-modal', h.label) });
    await a.locator('input:visible').last().fill(VICTIM);
    await a.getByRole('button', { name: 'Continue', exact: true }).click();
    await a.waitForTimeout(300);
    await a.locator('input[type=password]:visible').fill('wrong-password');
    await a.getByRole('button', { name: 'Confirm', exact: true }).click();
    await a.waitForTimeout(800);
    await a.screenshot({ path: shotOf('tb2-admin-password', h.label) });
    rec.admin_wrong_password = await a.locator('.text-red-500:visible').allInnerTexts();
    rec.victim_alive = psql(`select count(*) from users where email='${VICTIM}@example.test'`);
    await a.locator('input[type=password]:visible').fill(h.secret);
    await a.getByRole('button', { name: 'Confirm', exact: true }).click();
    await a.waitForTimeout(1500);
    rec.victim_gone = psql(`select count(*) from users where email='${VICTIM}@example.test'`);
    rec.victim_team = psql(`select count(*) from teams where name='${VICTIM}''s Team'`);
    await a.screenshot({ path: shotOf('tb2-admin-after', h.label) });
    rec.admin_audit = auditSince(start)
      .filter((r) => r.actor_email === `${ADMIN}@example.test` && !r.event.startsWith('auth.'))
      .map(scrub);
    await c.close();

    // Select team: several teams and no stored choice.
    tinker(`$t = App\\Models\\Team::create(["name"=>"${TEAM}","personal_team"=>false]); DB::table("team_user")->insert(["team_id"=>$t->id,"user_id"=>${owner},"role"=>"owner"]);`);
    // The first session must not restore the stored team meanwhile.
    await h.page.goto('about:blank');
    await h.page.waitForTimeout(500);
    psql(`update users set current_team_id = null where id=${owner}`);
    const c2 = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: 'dark' });
    const s = await c2.newPage();
    await s.goto(h.base + '/login');
    await s.fill('input[name=email]', `${OWNER}@example.test`);
    await s.fill('input[name=password]', h.secret);
    await s.click('button[type=submit]');
    await s.waitForTimeout(2500);
    rec.select_url = new URL(s.url()).pathname;
    await s.screenshot({ path: shotOf('tb2-select', h.label) });
    rec.select_text = (await s.locator('.auth-card').innerText()).replace(/\s+/g, ' ');
    rec.select_stored = psql(`select current_team_id from users where id=${owner}`);
    await s.goto(h.base + '/team');
    await s.waitForTimeout(1000);
    rec.select_guard = new URL(s.url()).pathname;
    await s.locator('.auth-card button', { hasText: TEAM }).click();
    await s.waitForTimeout(2000);
    rec.selected_url = new URL(s.url()).pathname;
    rec.selected_is_team = psql(`select t.name from users u join teams t on t.id=u.current_team_id where u.id=${owner}`);
    await s.goto(h.base + '/select-team');
    await s.waitForTimeout(1000);
    rec.select_with_team = new URL(s.url()).pathname;
    await c2.close();

    // Mail through the local mailpit: settings saved and restored by the caller.
    const host = h.label === 'ref' ? 'host.docker.internal' : '127.0.0.1';
    tinker(`$s = App\\Models\\InstanceSettings::find(0); $s->smtp_enabled = true; $s->smtp_host = "${host}"; $s->smtp_port = 18187; $s->smtp_from_address = "flow@example.test"; $s->smtp_from_name = "Flow"; $s->smtp_encryption = null; $s->smtp_username = null; $s->smtp_password = null; $s->save();`);
    await fetch('http://127.0.0.1:18188/api/v1/messages', { method: 'DELETE' });
    await h.go('/team/members');
    await h.shot('tb2-members-email');
    await h.page.locator('input[type=email]:visible').fill(`${OTHER}@example.test`);
    await h.button('Send email').click();
    rec.mail_toast = await h.toasts(2500);
    const list = await (await fetch('http://127.0.0.1:18188/api/v1/messages')).json();
    const m = list.messages?.[0];
    rec.mail_head = m && { subject: m.Subject.replace(TEAM, 'T'), to: m.To.map((t) => t.Address), from: m.From.Address };
    const body = m && (await (await fetch(`http://127.0.0.1:18188/api/v1/message/${m.ID}`)).json());
    rec.mail_html = body?.HTML.replace(/http:\/\/127\.0\.0\.1:\d+\/invitations\/[a-z0-9]+/g, 'LINK').replace(/\s+/g, ' ').replace(/v4\.[^<\s]*/g, 'V').length > 0;
    rec.mail_row = rows('team_invitations', `email='${OTHER}@example.test'`, 'role,via');
    restoreSmtp();

    // A new user through a magic link.
    await h.page.locator('input[type=email]:visible').fill(`${NEWUSER}@example.test`);
    await h.button('Generate link').click();
    await h.toasts();
    await h.page.waitForTimeout(500);
    const link = await h.page.locator('.data-table-row', { hasText: NEWUSER }).locator('span[title^="http"]').getAttribute('title');
    rec.magic_shape = link.replace(/token=.*/, 'token=T').replace(/127\.0\.0\.1:\d+/, 'HOST');
    const c3 = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: 'dark' });
    const n = await c3.newPage();
    await n.goto(h.base + '/auth/link?token=bogus');
    await n.waitForTimeout(1200);
    rec.magic_bad_url = new URL(n.url()).pathname;
    rec.magic_bad_text = await n.locator('main, body').first().innerText().then((t) => t.includes('Invitation has expired or been revoked.'));
    await n.screenshot({ path: shotOf('tb2-magic-bad', h.label) });
    await n.goto(link);
    await n.waitForTimeout(1200);
    await n.screenshot({ path: shotOf('tb2-magic', h.label) });
    rec.magic_page = (await n.locator('.auth-card').innerText()).replace(/\s+/g, ' ').replace('coolify-rs', 'Coolify');
    const hashBefore = psql(`select password from users where email='${NEWUSER}@example.test'`);
    await n.click('button:has-text("Accept invitation")');
    await n.waitForTimeout(2500);
    rec.magic_member = rows('team_user', `user_id=${id(NEWUSER)} and team_id in (select id from teams where personal_team=false)`, 'role');
    rec.magic_password_changed = hashBefore !== psql(`select password from users where email='${NEWUSER}@example.test'`);
    rec.magic_invitation_left = psql(`select count(*) from team_invitations where email='${NEWUSER}@example.test'`);
    rec.magic_force = psql(`select force_password_reset from users where email='${NEWUSER}@example.test'`);
    await n.screenshot({ path: shotOf('tb2-magic-after', h.label) });
    await n.goto(link);
    await n.waitForTimeout(1200);
    rec.magic_used_url = new URL(n.url()).pathname;
    await c3.close();

    rec.audit = mine(start, [`${OWNER}@example.test`, `${NEWUSER}@example.test`]);
    return rec;
  },
  {
    before: () => {
      cleanup();
      createUser(OWNER);
      createUser(OTHER);
      createUser(VICTIM);
      const admin = createUser(ADMIN);
      psql(`insert into team_user (team_id, user_id, role) values (0, ${admin}, 'admin')`);
      psql(`update users set current_team_id = (select team_id from team_user where user_id = ${admin} and team_id <> 0 limit 1) where id = ${admin}`);
      smtp = saveRow('instance_settings', 'id=0');
    },
    after: () => {
      restoreSmtp();
      cleanup();
    },
  },
);

