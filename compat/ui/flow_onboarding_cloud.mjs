// The provider cards of the onboarding wizard on both sides, with a fresh test user who has no tokens:
// each card opens `Server\New\By*` in its modal, at the token picker. No provider is contacted.
//   node compat/ui/flow_onboarding_cloud.mjs 18088
import { flow, psql } from './flow.mjs';
import { createUser, deleteUser, tinker } from './flow_test_users.mjs';

const [port] = process.argv.slice(2);
const user = 'screens2o';
const clean = (s) => s.replace(/\s+/g, ' ').trim();

const uid = createUser(user);
const team = psql(`select t.id from teams t join team_user tu on tu.team_id=t.id where tu.user_id=${uid} limit 1`);
psql(`update teams set show_boarding=true where id=${team}`);
tinker(`Illuminate\\Support\\Facades\\Cache::forget("user:${uid}:team:${team}");`);

try {
  await flow(port, user, async (h) => {
    const r = {};
    await h.page.route('**vultr.com**', (route) => route.abort());
    await h.page.waitForTimeout(1500);
    await h.page.getByRole('button', { name: 'Continue', exact: true }).first().click();
    await h.page.waitForTimeout(800);
    await h.shot('o-cards', true);
    for (const [card, title] of [['Hetzner Cloud', 'hetzner'], ['Vultr Cloud', 'vultr'], ['DigitalOcean', 'do'], ['Hostinger', 'hostinger']]) {
      await h.page.locator('h3:visible', { hasText: card }).first().click();
      await h.page.waitForTimeout(1200);
      const dialog = h.page.locator('.application-settings-section:visible header h3', { hasText: 'Connect a' }).last();
      r[`${title}.title`] = clean(await dialog.innerText());
      r[`${title}.body`] = clean(await dialog.locator('xpath=ancestor::*[contains(@class,"application-settings-section")][1]').innerText());
      r[`${title}.path`] = new URL(h.page.url()).pathname;
      await h.shot(`o-${title}`, false);
      await h.page.keyboard.press('Escape');
      await h.page.waitForTimeout(300);
      if (await dialog.isVisible().catch(() => false)) {
        await h.page.reload();
        await h.page.waitForTimeout(1200);
      }
    }
    return r;
  });
} finally {
  deleteUser(user);
}
