// Runtime logs section: both UIs read the same running application; compares the rows shown.
//   node compat/ui/flow_shared_logs.mjs <port> <runningAppUuid> <stoppedAppUuid>
import { flow } from './flow.mjs';

const [port, running, stopped] = process.argv.slice(2);
const path = (app) => `/project/j8xm2emlllhfnyebjzgyzp7z/environment/uj3st9yvtvzxh7rul3c3opcz/application/${app}/logs`;

await flow(port, 'alice', async (h) => {
  const rec = {};
  await h.go(path(stopped));
  await h.page.waitForTimeout(2000);
  await h.shot('sl-empty');
  rec.empty = (await h.page.locator('.empty-state').first().innerText()).replace(/\s+/g, ' ');
  await h.go(path(running));
  await h.page.waitForTimeout(3500);
  await h.shot('sl-running');
  const text = await h.page.locator('.runtime-logs-workspace').first().innerText();
  rec.has_nginx = /nginx/.test(text);
  return rec;
});
