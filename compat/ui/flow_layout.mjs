// The layout on both sides: command palette results, the selection steps, the deployments indicator,
// the dashboard's deployments and the popups. Creates one project with an application and queue rows
// in the test user's own team and removes them by id.
//   node compat/ui/flow_layout.mjs 18086 batd1
import { flow, psql } from './flow.mjs';
import { tinker } from './flow_test_users.mjs';

const [port, user] = process.argv.slice(2);
const team = psql(`select t.id from teams t join team_user tu on tu.team_id=t.id join users u on u.id=tu.user_id where u.email='${user}@example.test' and t.id<>0 order by t.id limit 1`);
const forget = () => tinker(`Illuminate\\Support\\Facades\\Cache::forget("global_search_items_${team}");`);

const created = tinker(`
$p = App\\Models\\Project::forceCreate(["name"=>"batd-project","team_id"=>${team}]);
$e = $p->environments()->first();
$a = App\\Models\\Application::forceCreate(["name"=>"batd-app","environment_id"=>$e->id,"git_repository"=>"x/y","git_branch"=>"main","build_pack"=>"static","ports_exposes"=>"80","destination_type"=>"App\\\\Models\\\\StandaloneDocker","destination_id"=>0,"fqdn"=>"https://batd-app.example.test"]);
$url = "/project/".$p->uuid."/environment/".$e->uuid."/application/".$a->uuid;
foreach (["in_progress","queued","finished","failed"] as $i => $st) {
  App\\Models\\ApplicationDeploymentQueue::forceCreate(["application_id"=>(string)$a->id,"application_name"=>"batd-app","deployment_uuid"=>"batd-dep-".$i,"deployment_url"=>$url."/deployment/batd-dep-".$i,"server_id"=>0,"server_name"=>"localhost","status"=>$st,"pull_request_id"=>$i===1?7:0,"destination_id"=>0]);
}
echo $p->id.",".$a->id.",".$e->id;`).split('\n').pop();
const [projectId, appId] = created.split(',');

function cleanup() {
  psql(`delete from application_deployment_queues where application_id='${appId}'`);
  tinker(`App\\Models\\Application::withTrashed()->find(${appId})?->forceDelete(); App\\Models\\Project::find(${projectId})?->delete();`);
  psql(`delete from application_settings where application_id=${appId}`);
  forget();
}

const results = async (h) =>
  h.page.$$eval('.search-result-item', (els) =>
    els.filter((e) => e.offsetParent !== null).map((e) => e.innerText.replace(/\s+/g, ' ').trim() + ' -> ' + (e.getAttribute('href') ?? '').replace(/^https?:\/\/[^/]+/, '')),
  );

async function typeQuery(h, q) {
  const input = h.page.locator('.command-palette-input');
  await input.fill('');
  await input.fill(q);
  await input.press('End');
  await h.page.waitForTimeout(500);
  return results(h);
}

try {
  await flow(
    port,
    user,
    async (h) => {
      const r = {};
      forget();
      await h.page.goto(h.base + '/');
      await h.page.waitForTimeout(2500);
      await h.shot('l-popups', false);
      r.popupText = await h.page.evaluate(() =>
        [...document.querySelectorAll('h2')].map((e) => e.innerText).filter((t) => /notifications enabled|Love Coolify/.test(t)),
      );
      await h.page.getByRole('button', { name: /accept and close/i }).first().click({ timeout: 2000 }).catch(() => {});
      await h.page.waitForTimeout(400);
      await h.shot('l-popups2', false);
      r.afterAccept = await h.page.evaluate(() => localStorage.getItem('popupNotification') !== null);
      await h.page.getByRole('button', { name: /maybe next time/i }).first().click({ timeout: 2000 }).catch(() => {});
      await h.page.waitForTimeout(400);
      r.afterMaybe = await h.page.evaluate(() => localStorage.getItem('popupSponsorship') !== null);

      // The dashboard with deployments, and the indicator.
      await h.go('/');
      await h.page.waitForTimeout(3500);
      await h.shot('l-dash', true);
      r.indicator = await h.page.locator('button[aria-label="Active deployments"]:visible').first().innerText();
      await h.page.locator('button[aria-label="Active deployments"]:visible').first().click();
      await h.page.waitForTimeout(400);
      await h.shot('l-indicator', false);
      await h.page.keyboard.press('Escape');

      // Palette through the shortcut and the search button.
      await h.page.keyboard.press('Control+k');
      await h.page.waitForSelector('.command-palette-input:visible');
      await h.page.waitForTimeout(800);
      await h.shot('l-palette', false);
      for (const q of ['dashboard', 'settings', 'server', 'app', 'db', 'project', 'zzzz-nothing', 'batd', 'batd-app.example', 'team', 'new', 'new post', 'postgres', 'wordpress', 'notif']) {
        r[`q:${q}`] = await typeQuery(h, q);
      }
      await typeQuery(h, 'batd');
      await h.shot('l-palette-batd', false);
      await typeQuery(h, 'new');
      await h.shot('l-palette-new', false);
      await typeQuery(h, 'zzzz-nothing');
      await h.shot('l-palette-empty', false);
      // Escape clears the query first, then closes.
      await h.page.keyboard.press('Escape');
      await h.page.waitForTimeout(250);
      r.afterEscape = await h.page.locator('.command-palette-input').inputValue();
      await h.page.keyboard.press('Escape');
      await h.page.waitForTimeout(300);
      r.closed = (await h.page.locator('.command-palette-input:visible').count()) === 0;

      // The selection steps of "new postgresql": alice's server is not reachable from this team.
      await h.page.keyboard.press('/');
      await h.page.waitForSelector('.command-palette-input:visible');
      await h.page.waitForTimeout(800);
      await h.page.locator('.command-palette-input').pressSequentially('new postgresql', { delay: 15 });
      await h.page.waitForTimeout(1200);
      r.step = (await h.page.locator('.command-palette-body').first().innerText()).replace(/\s+/g, ' ');
      await h.shot('l-palette-step', false);
      await h.page.keyboard.press('Escape');
      await h.page.waitForTimeout(300);
      await h.page.keyboard.press('Escape');
      await h.page.waitForTimeout(300);

      // "new project" opens its modal.
      await h.page.keyboard.press('/');
      await h.page.waitForSelector('.command-palette-input:visible');
      await h.page.waitForTimeout(800);
      await h.page.locator('.command-palette-input').pressSequentially('new project', { delay: 15 });
      await h.page.waitForTimeout(1000);
      r.projectModal = await h.page.locator('h3:visible', { hasText: 'New project' }).count();
      await h.shot('l-palette-project', false);
      await h.page.keyboard.press('Escape');

      // This page scope.
      await h.go('/profile');
      await h.page.keyboard.press('/');
      await h.page.waitForSelector('.command-palette-input:visible');
      await h.page.waitForTimeout(800);
      await h.page.keyboard.press('Tab');
      await h.page.waitForTimeout(300);
      r.pageScope = await results(h);
      await h.shot('l-palette-page', false);
      return r;
    },
    { after: () => {} },
  );
} finally {
  cleanup();
}
