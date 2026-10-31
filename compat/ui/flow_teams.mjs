// Team screens: new team, danger zone, member role changes, invitations and their acceptance, on
// both UIs with test users, comparing toasts, rows and audit_events. Nothing here uses alice, bob
// or carol except one read-only look and the instance admin deleting a test user.
//   node compat/ui/flow_teams.mjs [port]
import { flow, psql, auditMax, auditSince } from './flow.mjs';
import { createUser, deleteUser, tinker } from './flow_test_users.mjs';

const port = process.argv[2] ?? '18086';
const OWNER = 'batb1';
const MEMBER = 'batb2';
const INVITED = 'batb3';
const TEAM = 'Batb Flow Team';

const u = (name) => psql(`select id from users where email='${name}@example.test'`);
const rows = (table, where, cols) => psql(`select coalesce(json_agg(r), '[]') from (select ${cols} from ${table} where ${where} order by 1) r`);
const mask = (s, ids = {}) => {
  let t = typeof s === 'string' ? s : JSON.stringify(s);
  for (const [k, v] of Object.entries(ids)) t = t.split(String(v)).join(k);
  return t;
};
const audit = (start, ids) =>
  auditSince(start)
    .filter((r) => [ids.OWNER_ID, ids.MEMBER_ID].includes(r.actor_id) && !r.event.startsWith('auth.'))
    .map((r) => JSON.stringify(r).replace(/"invitation_uuid":"[a-z0-9]+"/g, '"invitation_uuid":"U"').replace(/"team_id":\d+/g, '"team_id":"T"').replace(/"(actor_id|member_id)":\d+/g, '"$1":"I"'));
const changesOf = (event, team) =>
  tinker(`echo json_encode(App\\Models\\AuditEvent::where("event","${event}")->where("team_id",${team})->latest("id")->first()?->changes);`).replace(/Batb Flow[^"]*/g, 'N');
const flowTeams = () => psql(`select coalesce(string_agg(id::text, ','), '') from teams where name like 'Batb Flow%'`);

function cleanup() {
  const ids = flowTeams();
  if (ids) {
    psql(`delete from team_invitations where team_id in (${ids})`);
    psql(`delete from team_user where team_id in (${ids})`);
    tinker(`App\\Models\\Team::whereIn("id",[${ids}])->get()->each->delete();`);
    psql(`delete from audit_events where team_id in (${ids})`);
  }
  for (const n of [OWNER, MEMBER, INVITED, 'batb4']) deleteUser(n);
}

/** Reloaded page state: the same on both sides once a Livewire action ended. */
const visible = (h) => h.page.locator('.application-settings-form').first().innerText().then((t) => t.replace(/\s+/g, ' ').trim());

await flow(
  port,
  OWNER,
  async (h) => {
    const rec = {};
    const owner = Number(u(OWNER));
    const member = Number(u(MEMBER));
    const ids = { OWNER_ID: owner, MEMBER_ID: member };
    let start = auditMax();

    // Team/Create.
    await h.go('/team');
    await h.shot('tb-general');
    await h.button('New team').click();
    await h.page.waitForTimeout(400);
    await h.shot('tb-new-modal', false);
    const form = h.page.locator('form:visible:has(button:has-text("Create team"))');
    await form.locator('input:visible').nth(0).fill('x');
    await form.locator('button:has-text("Create team")').click();
    rec.create_short = await h.toasts();
    await h.shot('tb-new-error', false);
    await form.locator('input:visible').nth(0).fill(TEAM);
    await form.locator('input:visible').nth(1).fill('Flow description');
    await form.locator('button:has-text("Create team")').click();
    await h.page.waitForTimeout(2500);
    rec.create_url = new URL(h.page.url()).pathname;
    rec.create_toasts = await h.toasts(200);
    rec.team_row = rows('teams', `name='${TEAM}'`, 'name,description,personal_team,show_boarding,is_mcp_server_enabled,is_build_server_fallback_enabled,is_two_factor_required,custom_server_limit');
    const team = Number(psql(`select id from teams where name='${TEAM}'`));
    ids.TEAM_ID = team;
    rec.changes_created = changesOf('ui.team.created', team);
    rec.team_user = mask(rows('team_user', `team_id=${team}`, 'user_id,role,created_at'), ids);
    rec.current_team = mask(psql(`select current_team_id from users where id=${owner}`), ids);
    rec.email_settings = rows('email_notification_settings', `team_id=${team}`, 'use_instance_email_settings,smtp_enabled');
    rec.settings_rows = psql(`select (select count(*) from discord_notification_settings where team_id=${team}), (select count(*) from slack_notification_settings where team_id=${team}), (select count(*) from telegram_notification_settings where team_id=${team}), (select count(*) from pushover_notification_settings where team_id=${team}), (select count(*) from webhook_notification_settings where team_id=${team})`);

    // Members, invite link.
    await h.go('/team/members');
    await h.shot('tb-members');
    rec.members_text = mask(await visible(h), ids);
    await h.page.locator('input[type=email]:visible').fill('not-an-email');
    await h.button('Generate link').click();
    rec.invite_invalid = await h.toasts();
    await h.shot('tb-invite-error', false);
    await h.page.locator('input[type=email]:visible').fill('');
    await h.button('Generate link').click();
    rec.invite_empty = await h.toasts();
    await h.page.locator('input[type=email]:visible').fill(`${OWNER}@example.test`);
    await h.button('Generate link').click();
    rec.invite_member = await h.toasts();

    // An existing user: batb2, then a new one: batb3.
    await h.page.locator('input[type=email]:visible').fill(`${MEMBER}@example.test`);
    await h.listbox('Role', 'Admin');
    await h.button('Generate link').click();
    rec.invite_existing = await h.toasts();
    await h.page.locator('input[type=email]:visible').fill(`${MEMBER}@example.test`);
    await h.button('Generate link').click();
    rec.invite_again = await h.toasts();
    await h.page.locator('input[type=email]:visible').fill(`${INVITED}@Example.test`);
    await h.listbox('Role', 'Member');
    await h.button('Generate link').click();
    rec.invite_new = await h.toasts();
    await h.page.waitForTimeout(500);
    await h.shot('tb-invitations');
    rec.invitations = rows('team_invitations', `team_id=${team}`, 'email,role,via,length(uuid) as uuid_len,left(link,9) as link_prefix');
    rec.invited_user = rows('users', `email='${INVITED}@example.test'`, 'name,email,force_password_reset,email_verified_at,marketing_emails,left(password,4) as pw');
    rec.invited_team = rows('teams', `id in (select team_id from team_user where user_id=(select id from users where email='${INVITED}@example.test'))`, 'name,personal_team,show_boarding');
    rec.link_shapes = (await h.page.locator('span[title^="http"]').allInnerTexts()).map((t) => t.replace(/token=.*/, 'token=T').replace(/[a-z0-9]{32}$/, 'UUID').replace(/127\.0\.0\.1:\d+/, 'HOST'));

    // Revoking a new invitee removes the provisional user.
    await h.page.locator('.data-table-row', { hasText: `${INVITED}@example.test` }).locator('button:has-text("Revoke")').click();
    rec.revoke_toasts = await h.toasts();
    rec.invited_gone = psql(`select count(*) from users where email='${INVITED}@example.test'`);

    // batb2 accepts in their own session.
    const inviteLink = await h.page.locator('span[title^="http"]').first().getAttribute('title');
    const ctx2 = await h.page.context().browser().newContext({ viewport: { width: 1440, height: 900 }, colorScheme: 'dark' });
    const p2 = await ctx2.newPage();
    await p2.addInitScript(() => localStorage.setItem('theme', 'dark'));
    await p2.goto(inviteLink);
    await p2.waitForTimeout(800);
    rec.invite_guest_redirect = new URL(p2.url()).pathname;
    await p2.fill('input[name=email]', `${MEMBER}@example.test`);
    await p2.fill('input[name=password]', h.secret);
    await p2.click('button[type=submit]');
    await p2.waitForTimeout(2000);
    await p2.goto(inviteLink);
    await p2.waitForTimeout(1200);
    await p2.screenshot({ path: new URL(`./shots/tb-invite-accept-${h.label}.png`, import.meta.url).pathname });
    rec.accept_page = (await p2.locator('.auth-card').innerText()).replace(/\s+/g, ' ').replace('coolify-rs', 'Coolify');
    await p2.click('button:has-text("Accept invitation")');
    await p2.waitForTimeout(2000);
    rec.accept_url = new URL(p2.url()).pathname;
    rec.after_accept = mask(rows('team_user', `team_id=${team}`, 'user_id,role'), ids);
    rec.invitation_left = psql(`select count(*) from team_invitations where team_id=${team}`);
    await p2.goto(inviteLink);
    await p2.waitForTimeout(800);
    rec.gone_page = (await p2.locator('body').innerText()).includes('404');
    await ctx2.close();

    // Team\Member actions, as the owner.
    await h.go('/team/members');
    const memberRow = h.page.locator('.data-table-row', { hasText: `${MEMBER}@example.test` });
    await memberRow.locator('button:has-text("Manage")').click();
    await h.page.waitForTimeout(200);
    await h.shot('tb-member-menu', false);
    await h.page.locator('.listbox-option:visible', { hasText: 'Make owner' }).click();
    await h.page.waitForTimeout(1500);
    await h.go('/team/members');
    rec.role_owner = psql(`select role from team_user where team_id=${team} and user_id=${member}`);
    await h.page.locator('.data-table-row', { hasText: `${MEMBER}@example.test` }).locator('button:has-text("Manage")').click();
    await h.page.locator('.listbox-option:visible', { hasText: 'Make member' }).click();
    await h.page.waitForTimeout(1500);
    await h.go('/team/members');
    rec.role_member = psql(`select role from team_user where team_id=${team} and user_id=${member}`);
    await h.page.locator('.data-table-row', { hasText: `${MEMBER}@example.test` }).locator('button:has-text("Manage")').click();
    await h.page.locator('.listbox-option:visible', { hasText: 'Remove member' }).click();
    await h.page.waitForTimeout(1500);
    rec.removed = mask(rows('team_user', `team_id=${team}`, 'user_id,role'), ids);

    // Audit log page.
    await h.go('/team/audit-log');
    await h.shot('tb-audit');
    rec.audit_page = (await h.page.locator('.data-table').first().innerText()).replace(/\s+/g, ' ').replace(/\d+ (second|minute)s? ago/g, 'AGO').replace(/batb\d/g, 'B');
    await h.page.locator('input[aria-label="Search activity"]').fill('role_updated');
    await h.page.waitForTimeout(1200);
    rec.audit_search = await h.page.locator('.data-table').first().locator('text=ui.team_member.role_updated').count();
    await h.page.locator('input[aria-label="Search activity"]').fill('nothing-matches-this');
    await h.page.waitForTimeout(1200);
    await h.shot('tb-audit-empty');
    await h.page.locator('input[aria-label="Search activity"]').fill('');
    await h.page.waitForTimeout(1000);

    // Danger zone: this team holds nothing, so it can go.
    await h.go('/team/danger');
    await h.shot('tb-danger');
    rec.danger_text = (await visible(h)).replace(TEAM, 'T');
    await h.button('Delete team').click();
    await h.page.waitForTimeout(400);
    await h.shot('tb-danger-modal', false);
    await h.page.locator('input:visible').last().fill(TEAM);
    await h.page.getByRole('button', { name: 'Permanently Delete' }).click();
    await h.page.waitForTimeout(2500);
    rec.delete_url = new URL(h.page.url()).pathname;
    rec.changes_deleted = changesOf('ui.team.deleted', team);
    rec.deleted_team = psql(`select count(*) from teams where id=${team}`);
    rec.dangling_pivot = mask(rows('team_user', `team_id=${team}`, 'user_id,role'), ids);
    rec.settings_after = psql(`select count(*) from email_notification_settings where team_id=${team}`);
    rec.current_after = psql(`select current_team_id is null from users where id=${owner}`);
    psql(`delete from team_user where team_id=${team}`);

    // Personal team: refused state.
    await h.go('/team/danger');
    await h.shot('tb-danger-personal');
    rec.danger_personal = (await visible(h)).replace(TEAM, 'T');

    rec.audit = audit(start, ids);
    psql(`delete from audit_events where team_id=${team}`);
    return rec;
  },
  {
    before: () => {
      cleanup();
      createUser(OWNER);
      createUser(MEMBER);
    },
    after: cleanup,
  },
);
