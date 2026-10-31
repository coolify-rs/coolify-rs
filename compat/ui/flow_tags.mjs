// Tags pages: same steps on both UIs, comparing toasts, rows and audit_events.
//   node compat/ui/flow_tags.mjs [port]
import { flow, psql, auditMax, auditSince } from './flow.mjs';

const port = process.argv[2] ?? '18086';
const NAME = 'flowtag';
const tagRow = () => psql(`select name, team_id from tags where name = '${NAME}'`);
const links = () => psql(`select count(*), string_agg(distinct taggable_type, ',') from taggables x join tags t on t.id = x.tag_id where t.name = '${NAME}'`);
const cleanup = () => psql(`delete from tags where name in ('${NAME}', 'flowtag2')`);
const maskUuid = (rows) => rows.map((r) => ({ ...r, metadata: { ...r.metadata, tag_uuid: 'U' } }));

console.log('== owner');
await flow(
  port,
  'alice',
  async (h) => {
    const rec = {};
    const start = auditMax();
    await h.go('/tags');
    await h.shot('tf-list-grid');
    await h.button('New tag').click();
    await h.page.waitForTimeout(300);
    await h.shot('tf-new-modal', false);
    const input = h.page.locator('input[placeholder="production"]');
    const errors = async (value) => {
      await input.fill(value);
      await h.button('Create tag').click();
      await h.page.waitForTimeout(700);
      return (await h.page.locator('.label-text-alt, .text-red-500').allInnerTexts()).join('|');
    };
    rec.e_space_toast = (await errors('two words'), await h.toasts());
    await h.shot('tf-new-error', false);
    await h.page.waitForTimeout(200);
    await input.fill(NAME);
    await h.button('Create tag').click();
    await h.page.waitForURL(`**/tags/${NAME}`);
    await h.page.waitForTimeout(1200);
    rec.created = tagRow();
    await h.shot('tf-show-empty');
    await h.page.locator('button:visible', { hasText: 'Connect to resources' }).first().click();
    await h.page.waitForTimeout(800);
    await h.shot('tf-connect', false);
    const first = h.page.locator('label:visible input[type=checkbox]').first();
    await first.click();
    await h.page.waitForTimeout(800);
    rec.linked = links();
    await h.page.keyboard.press('Escape');
    await h.page.locator('button[aria-label="Close"]:visible, button:has-text("Close"):visible').first().click({ timeout: 800 }).catch(() => {});
    await h.go(`/tags/${NAME}`);
    await h.shot('tf-show-resource');
    // Unlink again so that redeploy has nothing to deploy.
    await h.page.locator('button:visible', { hasText: 'Connect to resources' }).first().click();
    await h.page.waitForTimeout(800);
    await h.page.locator('label:visible input[type=checkbox]').first().click();
    await h.page.waitForTimeout(800);
    rec.unlinked = links();
    await h.go(`/tags/${NAME}`);
    await h.page.locator('button:visible', { hasText: 'Redeploy all' }).first().click();
    await h.page.waitForTimeout(400);
    await h.shot('tf-redeploy', false);
    await h.page.locator('input:visible[type=text], input:visible:not([type])').last().fill(NAME);
    await h.page.locator('button:visible', { hasText: 'Redeploy All' }).last().click();
    rec.redeploy = await h.toasts(1500);
    await h.go(`/tags/${NAME}`);
    await h.page.locator('button:visible', { hasText: /^\s*Delete\s*$/ }).first().click();
    await h.page.waitForTimeout(400);
    await h.shot('tf-delete', false);
    await h.page.locator('input:visible[type=text], input:visible:not([type])').last().fill(NAME);
    await h.page.locator('button:visible', { hasText: 'Delete tag' }).last().click();
    await h.page.waitForTimeout(1500);
    rec.deleted = tagRow();
    rec.url = new URL(h.page.url()).pathname;
    rec.audit = maskUuid(auditSince(start, '^ui\\.tag'));
    return rec;
  },
  { before: cleanup, after: cleanup },
);

console.log('== member and empty state');
await flow(port, 'bob', async (h) => {
  await h.go('/tags');
  await h.shot('tf-empty');
  await h.switchTeam("Bob Member's Team", 'Acme');
  await h.go('/tags');
  await h.shot('tf-member');
  const rec = { new_button: await h.page.locator('button:visible', { hasText: 'New tag' }).count() };
  return rec;
});
