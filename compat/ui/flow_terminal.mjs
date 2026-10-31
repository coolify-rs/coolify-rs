// /terminal on both sides: the targets of Acme, the filter, and the gate for a member (bob).
//   node compat/ui/flow_terminal.mjs 18086
import { flow } from './flow.mjs';

const [port] = process.argv.slice(2);
const list = (h) => h.page.locator('.terminal-target-card-list button:visible .command-palette-item-name').allInnerTexts();

await flow(port, 'alice', async (h) => {
  const r = {};
  await h.page.addInitScript(() => localStorage.setItem('popupNotification', String(Date.now())));
  await h.go('/terminal');
  await h.page.waitForTimeout(6000);
  r.groups = (await h.page.locator('.terminal-target-group-label:visible').allInnerTexts()).map((s) => s.replace(/\s+/g, ' '));
  r.targets = await list(h);
  await h.shot('t-list', true);
  await h.page.locator('input[placeholder="Filter targets"]').fill('proxy');
  await h.page.waitForTimeout(400);
  r.filtered = await list(h);
  await h.page.locator('input[placeholder="Filter targets"]').fill('zzz');
  await h.page.waitForTimeout(400);
  r.none = await h.page.locator('text=No matching targets').count();
  await h.shot('t-none', true);
  return r;
});

await flow(port, 'bob', async (h) => {
  const r = {};
  await h.page.goto(h.base + '/terminal');
  await h.page.waitForTimeout(2000);
  r.bobNav = await h.page.locator('.app-sidebar a[href="/terminal"]').count();
  await h.shot('t-bob', false);
  return r;
});
