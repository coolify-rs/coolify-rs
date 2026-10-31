// The user menu's What's New (SettingsDropdown), and the error pages of the team screens, with a
// test user. coolify-rs reads the same changelog file as the reference, copied to its data dir.
//   node compat/ui/flow_whatsnew.mjs [port]
import { flow, psql } from './flow.mjs';
import { createUser, deleteUser } from './flow_test_users.mjs';

const port = process.argv[2] ?? '18086';
const NAME = 'batb9';
const OTHER = 'batb10';

await flow(
  port,
  NAME,
  async (h) => {
    const rec = {};
    const id = Number(psql(`select id from users where email='${NAME}@example.test'`));
    await h.go('/team');
    const open = async () => {
      await h.page.locator('button[aria-label^="Account menu"]:visible').first().click();
      await h.page.waitForTimeout(400);
    };
    await open();
    await h.shot('wn-menu', false);
    rec.menu = (await h.page.locator('.top-user-menu-panel:visible').first().innerText()).replace(/\s+/g, ' ').replace(/How can we help\?|Feedback/g, '');
    await h.page.locator('button:visible', { hasText: "What's New" }).first().click();
    await h.page.waitForTimeout(800);
    await h.shot('wn-modal', false);
    rec.summary = (await h.page.locator('section.application-settings-section:has(input[placeholder="Search updates"])').first().innerText()).split('\n').slice(0, 8).join(' ').replace(/v4\.[^\s]*/, 'V');
    await h.page.locator('input[placeholder="Search updates"]').fill('nothing-matches-this');
    await h.page.waitForTimeout(300);
    await h.shot('wn-empty', false);
    await h.page.locator('input[placeholder="Search updates"]').fill('');
    await h.page.waitForTimeout(200);
    await h.page.locator('button[title="Mark as read"]:visible').first().click();
    await h.page.waitForTimeout(800);
    rec.read_rows = psql(`select count(*) from user_changelog_reads where user_id=${id}`);
    await h.button('Mark all read').click();
    await h.page.waitForTimeout(1000);
    rec.read_all = psql(`select count(*) > 0, count(*) = count(read_at) from user_changelog_reads where user_id=${id}`);
    await h.page.locator('button[aria-label="Close What\'s new"]').click();

    await h.go('/invitations/does-not-exist');
    await h.shot('er-404', false);
    rec.e404 = (await h.page.locator('body').innerText()).replace(/\s+/g, ' ');

    const other = Number(psql(`select id from users where email='${OTHER}@example.test'`));
    psql(`insert into team_invitations (team_id, uuid, email, role, link, via, created_at, updated_at) select team_id, 'batbinvite${h.label}', '${OTHER}@example.test', 'member', 'x', 'link', now(), now() from team_user where user_id=${id} limit 1`);
    void other;
    await h.go(`/invitations/batbinvite${h.label}`);
    await h.shot('er-400', false);
    rec.e400 = (await h.page.locator('body').innerText()).replace(/\s+/g, ' ');
    psql(`delete from team_invitations where uuid='batbinvite${h.label}'`);
    return rec;
  },
  {
    before: () => {
      deleteUser(NAME);
      deleteUser(OTHER);
      createUser(NAME);
      createUser(OTHER);
    },
    after: () => {
      deleteUser(NAME);
      deleteUser(OTHER);
    },
  },
);
